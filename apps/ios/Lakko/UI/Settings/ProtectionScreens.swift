// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

// ADR-0075 / docs/ux/identity-recovery.md section 19 — the screens of an
// account's protection:
//
//   * `LakkoConfirmDialog`     the app's own confirmation (never a system one);
//   * `ProtectAccountScreen`   "Protect your account" (19.1a): after sign-up,
//                              and behind the banner's `Protect it`;
//   * `RecoveryEmailScreen`    add / replace / remove the address (19.2);
//   * `SavePhraseScreen`       "Save a recovery phrase" (19.5);
//   * `AccountProtectionSection` the rows of Settings > Account;
//   * `AccountNoticeBanner`    what the account's devices are told (19.6);
//   * `SecureAccountScreen`    `It was not me`.
//
// What each shows is decided in `AccountProtection.swift`,
// `AccountNotices.swift` and `RecoveryEmailFlows.swift`; the views present.

// MARK: - The app's own confirmation

/// A confirmation drawn by the app over its own screen: a title, a body, a
/// primary and a plain button. Never a system alert.
struct LakkoConfirmDialog: View {
    let title: String
    let message: String
    let primaryTitle: String
    var primaryIsDestructive = false
    let secondaryTitle: String
    var busy = false
    let onPrimary: () -> Void
    let onSecondary: () -> Void

    var body: some View {
        ZStack {
            Color.black.opacity(0.4)
                .ignoresSafeArea()
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 14) {
                Text(title)
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                    .accessibilityAddTraits(.isHeader)
                Text(message)
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                if primaryIsDestructive {
                    Button(action: onPrimary) {
                        Text(primaryTitle)
                            .font(.body.weight(.medium))
                            .foregroundStyle(Color.lakkoDanger)
                            .frame(maxWidth: .infinity, minHeight: 44)
                            .overlay(
                                RoundedRectangle(cornerRadius: LakkoRadius.control)
                                    .stroke(Color.lakkoDanger, lineWidth: 1)
                            )
                    }
                    .buttonStyle(.plain)
                    .disabled(busy)
                } else {
                    AuthPrimaryButton(title: primaryTitle, busy: busy, action: onPrimary)
                }
                AuthSecondaryButton(title: secondaryTitle, disabled: busy, action: onSecondary)
            }
            .padding(24)
            .frame(maxWidth: 420)
            .background(Color.lakkoBackground)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
            .padding(24)
            .accessibilityAddTraits(.isModal)
        }
    }
}

// MARK: - Protect your account

/// "Protect your account" (UX 19.1a): the three second ways in (a recovery
/// email, an authenticator app, a passkey), of which one is required, and
/// the optional recovery phrase. No `Not now`: `Continue` is the only way
/// off the step and stays disabled until one of the three is done. The same
/// screen is the Settings entry (no `Continue`) and the step the landing
/// rule puts in front of the app (19.1c, `required`).
struct ProtectAccountScreen: View {
    @ObservedObject var center: AccountProtectionCenter
    /// The account has a username (a guest is offered neither the email nor
    /// a passkey).
    let hasUsername: Bool
    let onAddEmail: () -> Void
    let onAddAuthenticator: () -> Void
    let onAddPasskey: () -> Void
    let onSavePhrase: () -> Void
    /// Leave the step (`Continue`); `nil` for the Settings entry.
    var onLeave: (() -> Void)?
    /// The landing rule put the step here: its heading says so.
    var required = false
    /// A plain line under the passkey row (the sheet was cancelled, or the
    /// creation failed).
    var passkeyNote: (text: String, isError: Bool)?

    @ObservedObject private var l10n = Localization.shared

    private var layout: ProtectStep {
        ProtectStep.decide(
            offersEmail: ProtectStep.offersEmail(
                available: center.status?.available ?? false, hasUsername: hasUsername),
            // Every iOS version this app runs on can use passkeys.
            passkeysSupported: hasUsername,
            done: ProtectStep.done(factors: Self.knownFactors(center), phraseSaved: center.phraseSaved),
            keepsChats: center.keepsChats,
            leavable: onLeave != nil,
            required: required)
    }

    /// The factors as far as they are known: a part that was not read yet
    /// counts as absent on this screen (a row is offered, never hidden).
    static func knownFactors(_ center: AccountProtectionCenter) -> AccountFactors {
        AccountFactors(
            email: center.status?.enabled ?? false,
            authenticator: center.status?.totp ?? false,
            passkey: center.hasPasskey ?? false)
    }

    var body: some View {
        let layout = self.layout
        AuthScreen {
            AuthTitle(title: l10n.t(layout.titleKey), subtitle: l10n.t("protect.body"))

            card(layout.factors)

            // The email needs a username. A guest account never reaches this
            // step without one (it chooses it right before, ``GuestRoot``);
            // this is the session that does not know its name on this device.
            if !hasUsername, center.status?.available == true {
                AuthHint(text: l10n.t("recovery_email.needs_username"))
            }
            if let passkeyNote {
                AuthHint(text: passkeyNote.text, isError: passkeyNote.isError)
            }

            Text(l10n.t("protect.optional"))
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Color.lakkoTextMuted)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityAddTraits(.isHeader)
            card([layout.phrase])

            if let warning = layout.warning {
                ProtectWarningNote(text: l10n.t(warning.key))
            }
            if layout.showsPasswordOnlyNotice {
                PasswordOnlyNoticeView(
                    showsPasskeyNoSeal: PasswordOnlyNotice.showsPasskeyNoSeal(
                        factors: Self.knownFactors(center), seals: center.seals),
                    onSavePhrase: onSavePhrase,
                    onAddPasskey: hasUsername ? onAddPasskey : nil)
            }

            if case .continueOn(let enabled) = layout.footer {
                AuthPrimaryButton(title: l10n.t("protect.continue"), disabled: !enabled) { onLeave?() }
                    .accessibilityHint(enabled ? "" : l10n.t("protect.continue_needs"))
                if !enabled {
                    AuthHint(text: l10n.t("protect.continue_needs"))
                }
            }
        }
        .task { await center.refresh() }
    }

    private func card(_ items: [ProtectStep.Item]) -> some View {
        VStack(spacing: 0) {
            ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                row(item)
                if index < items.count - 1 {
                    Divider().padding(.leading, 56)
                }
            }
        }
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
    }

    @ViewBuilder
    private func row(_ item: ProtectStep.Item) -> some View {
        switch item.row {
        case .email:
            option(
                icon: .envelope, title: l10n.t("protect.email.title"),
                badge: l10n.t("protect.email.badge"), detail: l10n.t("protect.email.body"),
                done: item.done, action: onAddEmail)
        case .authenticator:
            option(
                icon: .smartphone, title: l10n.t("protect.totp.title"),
                badge: nil, detail: l10n.t("protect.totp.body"),
                done: item.done, action: onAddAuthenticator)
        case .passkey:
            option(
                icon: .passkey, title: l10n.t("protect.passkey.title"),
                badge: nil, detail: l10n.t("protect.passkey.body"),
                done: item.done, action: onAddPasskey)
        case .phrase:
            option(
                icon: .key, title: l10n.t("protect.phrase.title_optional"),
                badge: nil, detail: l10n.t("protect.phrase.body"),
                done: item.done, action: onSavePhrase)
        }
    }

    private func option(
        icon: LkIcon, title: String, badge: String?, detail: String, done: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 12) {
                LkIconView(icon, size: 20)
                    .frame(width: 28)
                    .foregroundStyle(Color.lakkoAccent)
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Text(title)
                            .font(.body.weight(.medium))
                            .foregroundStyle(Color.lakkoText)
                        if let badge, !done {
                            Text(badge)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(Color.lakkoAccent)
                        }
                    }
                    Text(detail)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.leading)
                }
                Spacer()
                if done {
                    Label(l10n.t("protect.done"), icon: .checkmarkCircleFill, size: 14, relativeTo: .footnote)
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Color.lakkoSuccess)
                } else {
                    LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .padding(.vertical, 12)
            .padding(.horizontal, 16)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // A row marked done is opened again only from Settings.
        .disabled(done)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(done ? "\(title), \(l10n.t("protect.done"))" : title)
    }
}

/// A note in the warning colour with the warning icon (UX 19.1a: the
/// authenticator as the only second way in). Not an error; announced once
/// when it appears.
struct ProtectWarningNote: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            LkIconView(.warning, size: 18, relativeTo: .footnote)
                .foregroundStyle(Color.lakkoWarning)
                .accessibilityHidden(true)
            Text(text)
                .font(.footnote)
                .foregroundStyle(Color.lakkoText)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(12)
        .background(Color.lakkoWarning.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .accessibilityElement(children: .combine)
        .onAppear { UIAccessibility.post(notification: .announcement, argument: text) }
    }
}

/// "Your chats depend on your password" (UX 19.1d): one copy, in the warning
/// colour, with its two actions. Drawn on the sign-up step and at the top of
/// Settings > Security > Recovery; the once-per-device dialog carries the
/// same title and body.
struct PasswordOnlyNoticeView: View {
    /// The account has a passkey and none keeps the chats.
    var showsPasskeyNoSeal = false
    let onSavePhrase: () -> Void
    /// `nil` where a passkey cannot be created.
    var onAddPasskey: (() -> Void)?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                LkIconView(.warning, size: 18, relativeTo: .subheadline)
                    .foregroundStyle(Color.lakkoWarning)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(l10n.t(PasswordOnlyNotice.titleKey))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.lakkoText)
                        .accessibilityAddTraits(.isHeader)
                    Text(l10n.t(PasswordOnlyNotice.bodyKey))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoText)
                    if showsPasskeyNoSeal {
                        Text(l10n.t(PasswordOnlyNotice.passkeyNoSealKey))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
            }
            HStack(spacing: 16) {
                Button(l10n.t(PasswordOnlyNotice.savePhraseKey), action: onSavePhrase)
                    .font(.footnote.weight(.semibold))
                    .frame(minHeight: 44)
                if let onAddPasskey {
                    Button(l10n.t(PasswordOnlyNotice.addPasskeyKey), action: onAddPasskey)
                        .font(.footnote.weight(.semibold))
                        .frame(minHeight: 44)
                }
            }
            .buttonStyle(.borderless)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoWarning.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .onAppear {
            UIAccessibility.post(
                notification: .announcement,
                argument: "\(l10n.t(PasswordOnlyNotice.titleKey)). \(l10n.t(PasswordOnlyNotice.bodyKey))")
        }
    }
}

// MARK: - Recovery email

/// Adding, replacing and removing the recovery email (UX 19.2): the address
/// step, then the code step on the same surface.
struct RecoveryEmailScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    /// Open on the removal confirmation (`Remove` in Settings).
    var startsRemoving = false
    /// The code was accepted, or the address was removed.
    let onDone: () -> Void

    @StateObject private var model: RecoveryEmailModel
    @StateObject private var proof: FreshAuthProofModel
    @ObservedObject private var resend: RetryCountdown
    @ObservedObject private var wait: RetryCountdown
    @State private var confirmingRemove = false
    /// UX 19.1c — the recovery email is the last second way in: the app's
    /// own dialog says so, and nothing is removed.
    @State private var lastFactor = false
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository,
        center: AccountProtectionCenter,
        startsRemoving: Bool = false,
        onDone: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.center = center
        self.startsRemoving = startsRemoving
        self.onDone = onDone
        let proof = FreshAuthProofModel(surface: .recoveryEmail, repository: authRepository)
        let replacing = center.status?.enabled ?? false
        let model = RecoveryEmailModel(
            ports: .live(authRepository: authRepository, center: center, proof: proof),
            replacing: replacing,
            // A new address is typed when one is replaced; after a recall
            // the address this device holds is confirmed by a letter again.
            prefill: replacing ? "" : (center.addressHeld ?? ""))
        self._model = StateObject(wrappedValue: model)
        self._proof = StateObject(wrappedValue: proof)
        self.resend = model.resend
        self.wait = model.wait
    }

    private var working: Bool { model.busy || proof.layout.working }

    var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("recovery_email.title"), subtitle: l10n.t("recovery_email.body"))
            if model.needsUsername {
                // The stored value is made from a username.
                AuthHint(text: l10n.t("recovery_email.needs_username"))
            } else if center.status?.available == false, center.status?.enabled != true {
                AuthHint(text: l10n.t("recovery_email.unavailable"), isError: true)
            } else {
                switch model.state.step {
                case .address: addressStep
                case .code: codeStep
                case .done: EmptyView()
                }
            }
        }
        .navigationTitle(l10n.t("recovery_email.title"))
        .navigationBarTitleDisplayMode(.inline)
        .overlay {
            if confirmingRemove {
                LakkoConfirmDialog(
                    title: l10n.t("recovery_email.remove.confirm.title"),
                    message: removeMessage,
                    primaryTitle: l10n.t("recovery_email.remove"),
                    primaryIsDestructive: true,
                    secondaryTitle: l10n.t("common.cancel"),
                    busy: working,
                    onPrimary: remove,
                    onSecondary: { confirmingRemove = false })
            }
        }
        .lakkoDialog(
            isPresented: Binding(
                get: { lastFactor },
                set: { shown in
                    guard !shown, lastFactor else { return }
                    lastFactor = false
                    // Opened only to remove: nothing else to do here.
                    if startsRemoving { onDone() }
                }),
            title: l10n.t(LastFactor.messageKey)
        ) {
            [.primary(l10n.t("common.ok")) {
                lastFactor = false
                if startsRemoving { onDone() }
            }]
        }
        .task {
            await proof.opened()
            await center.refresh()
            guard startsRemoving else { return }
            if LastFactor.refuses(removing: .email, factors: center.factors) {
                lastFactor = true
            } else {
                confirmingRemove = true
            }
        }
        .onChange(of: model.state.step) { _, step in
            if step == .done { onDone() }
        }
        .onDisappear {
            // The address and the code do not outlive the surface.
            model.closed()
            proof.closed()
        }
    }

    @ViewBuilder private var addressStep: some View {
        // UX 19.2 / 17.1 — the proof block above the email field when no
        // proof is live.
        FreshAuthBlock(model: proof, onSubmit: send)
        AuthTextField(
            label: l10n.t("recovery_email.field"),
            placeholder: "name@example.com",
            text: $model.address,
            keyboard: .emailAddress,
            contentType: .emailAddress,
            disabled: working,
            submitLabel: .go,
            onSubmit: send
        )
        if !model.address.isEmpty, !model.addressValid {
            AuthHint(text: l10n.t("recovery_email.invalid"), isError: true)
        }
        AuthHint(text: l10n.t("recovery_email.not_kept"))
        // The mailbox warning only while no phrase is saved.
        if !center.phraseSaved {
            AuthHint(text: l10n.t("recovery_email.mailbox_warning"))
        }
        // The authenticator's code is asked for as well as the mailed one.
        if center.status?.totp == true {
            AuthHint(text: l10n.t("recovery_email.totp_warning"))
        }
        line
        AuthPrimaryButton(
            title: l10n.t("recovery_email.send"),
            busy: working,
            busyTitle: l10n.t("common.sending"),
            disabled: !EmailEnrol.canSend(
                addressValid: model.addressValid, busy: working, waiting: wait.waiting,
                proofReady: proof.layout.ready),
            action: send
        )
    }

    @ViewBuilder private var codeStep: some View {
        Text(l10n.t("recovery_email.code.title"))
            .font(.headline)
            .foregroundStyle(Color.lakkoText)
            .accessibilityAddTraits(.isHeader)
        if let sentTo = model.sentTo {
            AuthHint(text: l10n.t("recovery_email.code.sent_to", texts: ["address": sentTo]))
        }
        AuthTextField(
            label: l10n.t("recovery_email.code.title"),
            placeholder: "123456",
            text: $model.code,
            keyboard: .numberPad,
            contentType: .oneTimeCode,
            disabled: working,
            submitLabel: .go,
            onSubmit: confirm
        )
        line
        AuthPrimaryButton(
            title: l10n.t("common.continue"),
            busy: working,
            busyTitle: l10n.t("common.checking"),
            disabled: EmailEnrol.normalizedCode(model.code) == nil,
            action: confirm
        )
        ResendRow(countdown: resend, disabled: working || wait.waiting, action: send)
        AuthLinkRow(title: l10n.t("recovery_email.code.other_address"), disabled: working) {
            model.useOtherAddress()
        }
    }

    @ViewBuilder private var line: some View {
        if let line = model.state.line {
            if let key = line.key {
                AuthHint(text: l10n.t(key), isError: true)
            } else {
                WaitNotice(countdown: wait)
            }
        }
    }

    private func send() {
        guard !working else { return }
        Task {
            // The proof first; a failure stays in the block.
            guard await proof.ensure() else { return }
            await model.send()
        }
    }

    private func confirm() {
        guard !working else { return }
        Task { await model.confirm() }
    }

    /// The confirmation's body; when the removal would leave only the
    /// authenticator app, the note of UX 19.1c before the buttons.
    private var removeMessage: String {
        let body = l10n.t("recovery_email.remove.confirm.body")
        guard let notice = AuthenticatorOnly.removalNoticeKey(
            removing: .email, factors: center.factors, phraseSaved: center.phraseSaved) else { return body }
        return body + "\n\n" + l10n.t(notice)
    }

    private func remove() {
        guard !working else { return }
        Task {
            guard await proof.ensure() else {
                // The block is on the address step: show it.
                confirmingRemove = false
                return
            }
            if await model.remove() {
                confirmingRemove = false
                onDone()
            } else {
                confirmingRemove = false
                // The server refused it as the last second way in.
                if model.state.line == .lastFactor { lastFactor = true }
            }
        }
    }
}

/// `Send again` / `Send again in 0:45`.
struct ResendRow: View {
    @ObservedObject var countdown: RetryCountdown
    var disabled = false
    let action: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if countdown.waiting {
            AuthHint(text: l10n.t(
                "recovery_email.code.resend_in",
                texts: ["time": ThreeWords.formatCountdown(remainingMs: countdown.remainingMs)]))
        } else {
            AuthLinkRow(title: l10n.t("recovery_email.code.resend"), disabled: disabled, action: action)
        }
    }
}

extension RecoveryEmailModel.Ports {

    /// The production ports.
    @MainActor
    static func live(
        authRepository: AuthRepository, center: AccountProtectionCenter, proof: FreshAuthProofModel
    ) -> RecoveryEmailModel.Ports {
        // The letter is written in the language of this device.
        let lang: @Sendable () async -> String = { await MainActor.run { Localization.shared.language } }
        return RecoveryEmailModel.Ports(
            loginHandle: { [weak authRepository] in
                authRepository.flatMap { DeviceSession.current($0)?.loginHandle }
            },
            sessionToken: { [weak authRepository] in authRepository?.identitySecrets?.sessionToken },
            grant: { [weak authRepository] in authRepository?.liveFreshGrant() },
            normalize: { try normalizeRecoveryEmail(address: $0) },
            start: { token, grant, handle, address in
                try await AuthClient().recoveryEmailEnrolStart(
                    sessionToken: token, freshGrant: grant, handle: handle, address: address, lang: await lang())
            },
            finish: { token, challengeId, code in
                try await AuthClient().recoveryEmailEnrolFinish(
                    sessionToken: token, challengeId: challengeId, code: code)
            },
            remove: { token, grant in
                try await AuthClient().recoveryEmailRemove(sessionToken: token, freshGrant: grant)
            },
            enrolled: { [weak center] result, replaced, address in
                center?.enrolled(result, replaced: replaced, address: address.isEmpty ? nil : address)
            },
            removed: { [weak center] in center?.removed() },
            proofExpired: { [weak authRepository, weak proof] in
                authRepository?.dropFreshAuth()
                proof?.proofExpired()
            }
        )
    }
}

// MARK: - Save a recovery phrase

/// What the save-phrase screen shows, pure (UX 19.5, amended 2026-11-26:
/// saving a phrase changes no key, since no copy of it ever left the
/// device).
enum SavePhraseState: Equatable {
    /// The explanation and `Show my 12 words`.
    case idle
    /// "We could not finish. Nothing changed." with `Try again`: this
    /// device holds no phrase that is the account's.
    case failed
    /// The words this device already holds. The account counts as protected
    /// only once the three-word check passed and its published protection
    /// lists the phrase.
    case heldWords(String)
}

/// The flow behind ``SavePhraseScreen`` (UX 19.5), over injected ports: the
/// words the device holds, the three-word check, then the account's
/// protection is published again with the phrase in it (ADR-0101 section
/// 10), which ends the notice of 19.1d.
@MainActor
final class SavePhraseFlow: ObservableObject {

    struct Ports {
        /// The phrase this device holds, when it is the account's.
        var heldPhrase: @MainActor () -> String?
        /// Publish the account's protection with the phrase in it. `false`
        /// when it could not be published now: nothing counts as saved.
        var publishPhraseSeal: @MainActor () async -> Bool
    }

    @Published private(set) var state: SavePhraseState = .idle
    /// The last step is running.
    @Published private(set) var confirming = false
    /// The last step failed: `protect.phrase.save.failed` with `Try again`.
    @Published private(set) var confirmFailed = false

    private let ports: Ports

    init(ports: Ports) {
        self.ports = ports
    }

    /// `Show my 12 words` / `Try again`. The caller obtained the proof.
    func primary() {
        if let phrase = ports.heldPhrase() {
            state = .heldWords(phrase)
        } else {
            state = .failed
        }
    }

    /// The three-word check passed: publish. `true` once the account's
    /// protection lists the phrase; the screen then closes.
    func confirmHeldWords() async -> Bool {
        guard case .heldWords = state, !confirming else { return false }
        confirming = true
        confirmFailed = false
        defer { confirming = false }
        if await ports.publishPhraseSeal() { return true }
        confirmFailed = true
        return false
    }
}

extension SavePhraseFlow.Ports {

    /// The production ports.
    @MainActor
    static func live(authRepository: AuthRepository, center: AccountProtectionCenter) -> SavePhraseFlow.Ports {
        SavePhraseFlow.Ports(
            heldPhrase: { [weak authRepository] in authRepository?.heldPhraseToSave() },
            publishPhraseSeal: { [weak center] in await center?.savePhraseSeal() ?? false }
        )
    }
}

/// Settings > Account > Recovery phrase, and the step of 19.1a (UX 19.5):
/// the explanation, the proof, the words this device holds, the three-word
/// check.
struct SavePhraseScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    let onDone: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @StateObject private var flow: SavePhraseFlow
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository, center: AccountProtectionCenter,
        onDone: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.center = center
        self.onDone = onDone
        let proof = FreshAuthProofModel(surface: .savePhrase, repository: authRepository)
        self._proof = StateObject(wrappedValue: proof)
        self._flow = StateObject(wrappedValue: SavePhraseFlow(
            ports: .live(authRepository: authRepository, center: center)))
    }

    var body: some View {
        // One container, so the screen opens once and closes once however
        // often the state swaps what is inside it.
        ZStack {
            switch flow.state {
            case .heldWords(let phrase):
                RecoveryPhraseGuide(
                    phrase: phrase,
                    title: l10n.t("recovery_phrase.save.title"),
                    intro: l10n.t("recovery_phrase.save.lede"),
                    doneTitle: flow.confirmFailed ? l10n.t("common.try_again") : nil,
                    doneBusy: flow.confirming,
                    confirmFooter: flow.confirmFailed
                        ? AnyView(AuthHint(text: l10n.t("protect.phrase.save.failed"), isError: true)) : nil,
                    extra: { AuthHint(text: l10n.t("recovery_phrase.save.anyone_with_words")) },
                    onDone: confirmHeldWords
                )
            case .idle, .failed:
                explanation
            }
        }
        .task { await proof.opened() }
        .onDisappear { proof.closed() }
    }

    private var explanation: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("protect.phrase.title"),
                subtitle: l10n.t("protect.phrase.save.body"))
            if flow.state == .failed {
                AuthHint(text: l10n.t("protect.phrase.save.failed"), isError: true)
            } else {
                FreshAuthBlock(model: proof, onSubmit: save)
            }
            AuthPrimaryButton(
                title: l10n.t(flow.state == .idle ? "protect.phrase.save.show" : "common.try_again"),
                busy: proof.layout.working,
                busyTitle: l10n.t("common.checking"),
                disabled: flow.state == .idle && !proof.layout.ready,
                action: save
            )
        }
        .navigationTitle(l10n.t("protect.phrase.title"))
        .navigationBarTitleDisplayMode(.inline)
    }

    private func save() {
        guard !proof.layout.working else { return }
        Task {
            guard await proof.ensure() else { return }
            flow.primary()
        }
    }

    /// The three words passed. Nothing counts as saved before the account's
    /// protection was published with the phrase in it.
    private func confirmHeldWords() {
        guard !flow.confirming else { return }
        Task {
            if await flow.confirmHeldWords() { onDone() }
        }
    }
}

// MARK: - Settings > Account

/// The protection rows of Settings > Account: the status line (what keeps
/// the chats, UX 19.1c), `Recovery email`, `Recovery phrase`.
struct AccountProtectionSection: View {
    @ObservedObject var center: AccountProtectionCenter
    let hasUsername: Bool
    let onOpen: (TabRoute) -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let rows = center.rows {
            HStack {
                Label(l10n.t(rows.statusKey), icon: rows.statusIsWarning ? .warning : .lockShield)
                    .foregroundStyle(rows.statusIsWarning ? Color.lakkoWarning : Color.lakkoText)
                Spacer()
            }
            if rows.statusIsWarning {
                // UX 19.1c — the status line of an account kept by its
                // password only carries the 19.1d actions.
                HStack(spacing: 16) {
                    Button(l10n.t(PasswordOnlyNotice.savePhraseKey)) { onOpen(.savePhrase) }
                        .font(.footnote.weight(.semibold))
                        .frame(minHeight: 44)
                    if hasUsername {
                        Button(l10n.t(PasswordOnlyNotice.addPasskeyKey)) { onOpen(.passkeys) }
                            .font(.footnote.weight(.semibold))
                            .frame(minHeight: 44)
                    }
                }
                .buttonStyle(.borderless)
            }
            switch rows.email {
            case .absent:
                EmptyView()
            case .off:
                valueRow(
                    title: l10n.t("recovery_email.title"), icon: .envelope,
                    value: l10n.t("recovery_email.status.off")
                ) { onOpen(.recoveryEmail(remove: false)) }
            case .on:
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Label(l10n.t("recovery_email.title"), icon: .envelope)
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                        Text(l10n.t("recovery_email.status.on"))
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    // The address, on a device that holds it (plain text
                    // under the row title); a device that does not shows On.
                    if let address = center.addressHeld {
                        Text(verbatim: address)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .textSelection(.enabled)
                    }
                    HStack(spacing: 16) {
                        Button(l10n.t("recovery_email.replace")) { onOpen(.recoveryEmail(remove: false)) }
                            .font(.footnote.weight(.semibold))
                            .frame(minHeight: 44)
                        Button(l10n.t("recovery_email.remove")) { onOpen(.recoveryEmail(remove: true)) }
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Color.lakkoDanger)
                            .frame(minHeight: 44)
                    }
                    .buttonStyle(.borderless)
                }
            }
            switch rows.phrase {
            case .save:
                valueRow(
                    title: l10n.t("protect.phrase.title"), icon: .key, value: nil
                ) { onOpen(.savePhrase) }
            case .show:
                valueRow(
                    title: l10n.t("recovery_phrase.label"), icon: .key, value: nil
                ) { onOpen(.recoveryFactors) }
            }
        }
    }

    private func valueRow(
        title: String, icon: LkIcon, value: String?, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack {
                Label(title, icon: icon)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                if let value {
                    Text(value).foregroundStyle(Color.lakkoTextMuted)
                }
                LkIconView(.chevronForward, size: 14, relativeTo: .footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Notices

/// What the account's devices are told (UX 19.2, 19.5, 19.6): one notice at
/// a time above the chat list, with `It was me` / `It was not me`.
struct AccountNoticeBanner: View {
    @ObservedObject var center: AccountProtectionCenter
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        if let notice = center.notices.first, let kind = notice.known {
            let copy = AccountNoticeCopy.of(kind)
            let date = RecallCopy.date(ms: notice.atMs, locale: l10n.locale)
            HStack(alignment: .top, spacing: 12) {
                LkIconView(.shieldExclamation, size: 20, relativeTo: .subheadline)
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(l10n.t(copy.titleKey))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.lakkoText)
                    if let bodyKey = copy.bodyKey {
                        Text(l10n.t(bodyKey, texts: ["date": date]))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    HStack(spacing: 16) {
                        Button(l10n.t(AccountNoticeCopy.wasMeKey)) { center.acknowledge(notice) }
                            .font(.footnote.weight(.semibold))
                            .frame(minHeight: 44)
                        Button(l10n.t(AccountNoticeCopy.wasNotMeKey)) { center.wasNotMe(notice) }
                            .font(.footnote.weight(.semibold))
                            .frame(minHeight: 44)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(Color.lakkoSurface)
        }
    }
}

/// `It was not me` (UX 19.6): one screen (`notice.secure.title`) with, in
/// order, a new password, `Remove`, and, when no phrase is saved, `Save a
/// recovery phrase`.
///
/// Reached from a notice, the password form is open for ten minutes without
/// another proof (``AuthRepository/openPasswordChangeForNotice()``, called
/// by the notice's `It was not me`, never by this screen): this device holds
/// the keys, and the password in force may be one the person never chose.
/// Outside that window the ordinary gate stands in front of it. Removing
/// the email and saving a phrase ask for their own proof.
struct SecureAccountScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    let onOpen: (TabRoute) -> Void

    @State private var passwordSaved = false
    @State private var gateOpen = false
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        let actions = SecureAccountActions.offered(center.status, phraseSaved: center.phraseSaved)
        AuthScreen {
            AuthTitle(title: l10n.t("notice.secure.title"))
            if authRepository.canReplacePassword {
                if passwordSaved {
                    AuthHint(text: l10n.t("notice.secure.password_saved"))
                } else if gateOpen {
                    passwordForm { gateOpen = authRepository.passwordChangeAllowed }
                } else {
                    PasswordChangeGateView(authRepository: authRepository) { relock in
                        passwordForm(onProofNeeded: relock)
                    }
                }
            }
            if actions.contains(.removeEmail) {
                AuthSecondaryButton(title: l10n.t("recovery_email.remove"), icon: .envelope) {
                    onOpen(.recoveryEmail(remove: true))
                }
            }
            if actions.contains(.savePhrase) {
                AuthSecondaryButton(title: l10n.t("protect.phrase.title"), icon: .key) {
                    onOpen(.savePhrase)
                }
            }
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { gateOpen = authRepository.passwordChangeAllowed }
        .task { await center.refresh() }
    }

    private func passwordForm(onProofNeeded: @escaping () -> Void) -> some View {
        NewPasswordForm(
            authRepository: authRepository,
            saveTitle: l10n.t("recovery.new_password.save"),
            onProofNeeded: onProofNeeded,
            onSaved: { passwordSaved = true }
        )
    }
}
