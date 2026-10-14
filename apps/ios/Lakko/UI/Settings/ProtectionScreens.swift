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

/// "Protect your account" (UX 19.1a): `Add an email`, the passkey row,
/// `Save a recovery phrase`; rows already done are marked. Under them `Not
/// now` (the app's own confirmation) until something is done, then
/// `Continue`. The step after sign-up, and the target of the banner's
/// `Protect it`. What it shows is ``ProtectStep``.
struct ProtectAccountScreen: View {
    @ObservedObject var center: AccountProtectionCenter
    /// The account has a username (a guest is not offered the email).
    let hasUsername: Bool
    let onAddEmail: () -> Void
    let onAddPasskey: () -> Void
    let onSavePhrase: () -> Void
    /// Leave the step (`Not now`, confirmed, or `Continue`); `nil` for the
    /// Settings entry.
    var onLeave: (() -> Void)?

    @State private var confirmingSkip = false
    @ObservedObject private var l10n = Localization.shared

    private var offersEmail: Bool {
        ProtectionRows.signupOffersEmail(
            available: center.status?.available ?? false, hasUsername: hasUsername)
    }

    private var layout: ProtectStep {
        ProtectStep.decide(
            offersEmail: offersEmail,
            // Every iOS version this app runs on can use passkeys.
            passkeysSupported: true,
            done: ProtectStep.done(status: center.status, hasPasskey: center.hasPasskey == true),
            leavable: onLeave != nil)
    }

    var body: some View {
        let layout = self.layout
        AuthScreen {
            AuthTitle(title: l10n.t("protect.title"), subtitle: l10n.t("protect.body"))

            VStack(spacing: 0) {
                ForEach(Array(layout.items.enumerated()), id: \.offset) { index, item in
                    row(item)
                    if index < layout.items.count - 1 {
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

            // The email needs a username. A guest account never reaches this
            // step without one (it chooses it right before, ``GuestRoot``);
            // this is the session that does not know its name on this device.
            if !hasUsername, center.status?.available == true {
                AuthHint(text: l10n.t("recovery_email.needs_username"))
            }

            switch layout.footer {
            case .notNow:
                AuthLinkRow(title: l10n.t("protect.skip"), action: { confirmingSkip = true })
            case .continueOn:
                AuthPrimaryButton(title: l10n.t("protect.continue")) { onLeave?() }
            case .none:
                EmptyView()
            }
        }
        .overlay {
            if confirmingSkip, let onLeave {
                LakkoConfirmDialog(
                    title: l10n.t("protect.skip.confirm.title"),
                    message: l10n.t("protect.skip.confirm.body"),
                    primaryTitle: l10n.t(offersEmail ? "protect.email.title" : "protect.phrase.title"),
                    secondaryTitle: l10n.t("protect.skip.confirm.continue"),
                    onPrimary: {
                        confirmingSkip = false
                        if offersEmail { onAddEmail() } else { onSavePhrase() }
                    },
                    onSecondary: {
                        confirmingSkip = false
                        onLeave()
                    })
            }
        }
        .task { await center.refresh() }
    }

    @ViewBuilder
    private func row(_ item: ProtectStep.Item) -> some View {
        switch item.row {
        case .email:
            option(
                systemImage: "envelope", title: l10n.t("protect.email.title"),
                badge: l10n.t("protect.email.badge"), detail: l10n.t("protect.email.body"),
                done: item.done, action: onAddEmail)
        case .passkey:
            option(
                systemImage: "person.badge.key", title: l10n.t("protect.passkey.title"),
                badge: nil, detail: l10n.t("protect.passkey.body"),
                done: item.done, action: onAddPasskey)
        case .phrase:
            option(
                systemImage: "key", title: l10n.t("protect.phrase.title"),
                badge: nil, detail: l10n.t("protect.phrase.body"),
                done: item.done, action: onSavePhrase)
        }
    }

    private func option(
        systemImage: String, title: String, badge: String?, detail: String, done: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: systemImage)
                    .font(.body)
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
                    Label(l10n.t("protect.done"), systemImage: "checkmark.circle.fill")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Color.lakkoSuccess)
                } else {
                    Image(systemName: "chevron.forward")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .padding(.vertical, 12)
            .padding(.horizontal, 16)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(done)
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
            prefill: EmailReseal.prefill(replacing: replacing, addressHeld: center.addressHeld))
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
                    message: l10n.t("recovery_email.remove.confirm.body"),
                    primaryTitle: l10n.t("recovery_email.remove"),
                    primaryIsDestructive: true,
                    secondaryTitle: l10n.t("common.cancel"),
                    busy: working,
                    onPrimary: remove,
                    onSecondary: { confirmingRemove = false })
            }
        }
        .task {
            await proof.opened()
            await center.refresh()
            if startsRemoving { confirmingRemove = true }
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
            phraseSaved: { [weak center] in center?.phraseSaved ?? false },
            normalize: { try normalizeRecoveryEmail(address: $0) },
            start: { token, grant, handle, address in
                try await AuthClient().recoveryEmailEnrolStart(
                    sessionToken: token, freshGrant: grant, handle: handle, address: address, lang: await lang())
            },
            seal: { [weak authRepository] handle, address, salt in
                // The phrase this device holds, sealed off the main thread.
                guard let phrase = await authRepository?.heldPhraseForEmailVault() else {
                    throw SessionNotReady()
                }
                return try await EmailVaultWork.seal(
                    phrase: phrase, handle: handle, address: address, vaultSalt: salt)
            },
            finish: { token, challengeId, code, sealed in
                try await AuthClient().recoveryEmailEnrolFinish(
                    sessionToken: token, challengeId: challengeId, code: code, sealedEntropy: sealed)
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

/// What the save-phrase screen shows, pure.
enum SavePhraseState: Equatable {
    /// The explanation and `Show my 12 words`.
    case idle
    /// The key change is running: a progress line, no words.
    case working
    /// "We could not finish. Nothing changed." with `Try again`.
    case failed
    /// The keys were changed; publishing is still owed: the words are
    /// withheld and `Try again` finishes it.
    case publishOwed
    /// The change went through: the new words.
    case words(String)

    /// What the rotation's result means on this screen. The words appear
    /// only once the server step succeeded and nothing withholds them.
    static func after(_ result: KeyRotationPlan.Result) -> SavePhraseState {
        switch result {
        case .rotated(let phrase): return .words(phrase)
        case .rotatedPublishOwed: return .publishOwed
        // Never the answer of the save-phrase flow (it always shows its
        // words); read as owed so nothing is claimed.
        case .rotatedNoWords: return .publishOwed
        case .freshAuthRequired: return .idle
        case .removedNotRotated, .notRemoved: return .failed
        }
    }
}

/// "Save a recovery phrase" (UX 19.5): the ADR-0074 key rotation with no
/// device removed, then the existing words grid and three-word check.
struct SavePhraseScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    let rotation: KeyRotationRunner
    /// The words were confirmed.
    let onDone: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @State private var state: SavePhraseState = .idle
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository, center: AccountProtectionCenter, rotation: KeyRotationRunner,
        onDone: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.center = center
        self.rotation = rotation
        self.onDone = onDone
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .savePhrase, repository: authRepository))
    }

    private var working: Bool { state == .working || proof.layout.working }

    var body: some View {
        if case .words(let phrase) = state {
            RecoveryPhraseGuide(
                phrase: phrase,
                title: l10n.t("recovery_phrase.save.title"),
                intro: l10n.t("recovery_phrase.save.lede"),
                extra: { AuthHint(text: l10n.t("recovery_phrase.save.anyone_with_words")) },
                onDone: {
                    rotation.acknowledgePhrase()
                    Task { await center.refresh() }
                    onDone()
                }
            )
        } else {
            AuthScreen {
                AuthTitle(
                    title: l10n.t("protect.phrase.title"),
                    subtitle: l10n.t("protect.phrase.save.body"))
                AuthHint(text: l10n.t("protect.phrase.save.effect"))
                switch state {
                case .idle, .working, .words:
                    FreshAuthBlock(model: proof, onSubmit: save)
                case .failed:
                    AuthHint(text: l10n.t("protect.phrase.save.failed"), isError: true)
                case .publishOwed:
                    AuthHint(text: l10n.t("protect.phrase.save.finishing"), isError: true)
                }
                AuthPrimaryButton(
                    title: l10n.t(state == .idle ? "protect.phrase.save.show" : "common.try_again"),
                    busy: working,
                    busyTitle: l10n.t("protect.phrase.save.progress"),
                    disabled: state == .idle && !proof.layout.ready,
                    action: save
                )
            }
            .navigationTitle(l10n.t("protect.phrase.title"))
            .navigationBarTitleDisplayMode(.inline)
            .task {
                await proof.opened()
                // A change that already went through (the app was closed
                // before the words were confirmed): finish it, never start
                // a second one.
                rotation.reload()
                if rotation.resume == .showWords || rotation.resume == .finishOwed
                    || rotation.resume == .commitLandedUnseen {
                    await resumeOwed()
                }
            }
            .onDisappear { proof.closed() }
        }
    }

    private func save() {
        guard !working else { return }
        if state == .publishOwed {
            Task { await resumeOwed() }
            return
        }
        Task {
            guard await proof.ensure() else { return }
            state = .working
            let result = await rotation.savePhrase()
            if result == .freshAuthRequired {
                authRepository.dropFreshAuth()
                proof.proofExpired()
            }
            state = SavePhraseState.after(result)
        }
    }

    /// Finish what a committed change still owes; show the words when
    /// nothing withholds them.
    private func resumeOwed() async {
        state = .working
        await rotation.resumeOwed()
        if let phrase = rotation.wordsToShow() {
            state = .words(phrase)
        } else {
            state = rotation.resume == .nothing ? .idle : .publishOwed
        }
    }
}

// MARK: - Settings > Account

/// The protection rows of Settings > Account: the status line, the banner
/// of an unprotected account, `Recovery email`, `Recovery phrase`.
struct AccountProtectionSection: View {
    @ObservedObject var center: AccountProtectionCenter
    let hasUsername: Bool
    let onOpen: (TabRoute) -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        // Always present (even before the first status is known), so that
        // opening Settings reads the account's protection again.
        Color.clear
            .frame(height: 0)
            .listRowInsets(EdgeInsets())
            .listRowSeparator(.hidden)
            .accessibilityHidden(true)
            .task { await refreshOnOpen() }
        if let rows = center.rows {
            HStack {
                Label(l10n.t(rows.statusKey), systemImage: "lock.shield")
                    .foregroundStyle(Color.lakkoText)
                Spacer()
            }
            if center.bannerVisible, let status = center.status, let copy = center.bannerCopy {
                // UX 19.1a — one dismissible banner, here and nowhere else.
                // After a reset, a recall or a rotation dropped the email
                // vault it asks for the email again.
                VStack(alignment: .leading, spacing: 8) {
                    Text(l10n.t(copy.titleKey))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoText)
                    if let bodyKey = copy.bodyKey {
                        Text(l10n.t(bodyKey))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    HStack(spacing: 16) {
                        Button(l10n.t(copy.actionKey)) {
                            onOpen(
                                status.needsAddingAgain(addressHeld: center.addressHeld != nil)
                                    ? .recoveryEmail(remove: false) : .protectAccount)
                        }
                            .font(.footnote.weight(.semibold))
                            .frame(minHeight: 44)
                        Button(l10n.t("common.dismiss")) { center.dismissBanner() }
                            .font(.footnote)
                            .frame(minHeight: 44)
                    }
                    .buttonStyle(.borderless)
                }
            }
            switch rows.email {
            case .absent:
                EmptyView()
            case .addAgain:
                valueRow(
                    title: l10n.t("recovery_email.title"), systemImage: "envelope",
                    value: l10n.t("recovery_email.add_again.title")
                ) { onOpen(.recoveryEmail(remove: false)) }
            case .off:
                valueRow(
                    title: l10n.t("recovery_email.title"), systemImage: "envelope",
                    value: l10n.t("recovery_email.status.off")
                ) { onOpen(.recoveryEmail(remove: false)) }
            case .on:
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Label(l10n.t("recovery_email.title"), systemImage: "envelope")
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
                    title: l10n.t("protect.phrase.title"), systemImage: "key", value: nil
                ) { onOpen(.savePhrase) }
            case .show:
                valueRow(
                    title: l10n.t("recovery_phrase.label"), systemImage: "key", value: nil
                ) { onOpen(.recoveryFactors) }
            }
        }
    }

    /// Settings opened: the phrase-saved flag and the rows follow the server.
    private func refreshOnOpen() async { await center.refresh() }

    private func valueRow(
        title: String, systemImage: String, value: String?, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack {
                Label(title, systemImage: systemImage)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                if let value {
                    Text(value).foregroundStyle(Color.lakkoTextMuted)
                }
                Image(systemName: "chevron.forward")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
            }
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
                Image(systemName: "exclamationmark.shield")
                    .font(.subheadline)
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
        let actions = SecureAccountActions.offered(center.status)
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
                AuthSecondaryButton(title: l10n.t("recovery_email.remove"), systemImage: "envelope") {
                    onOpen(.recoveryEmail(remove: true))
                }
            }
            if actions.contains(.savePhrase) {
                AuthSecondaryButton(title: l10n.t("protect.phrase.title"), systemImage: "key") {
                    onOpen(.savePhrase)
                }
                AuthHint(text: l10n.t("notice.secure.new_keys"))
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
