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
                icon: .envelope, title: l10n.t("protect.email.title"),
                badge: l10n.t("protect.email.badge"), detail: l10n.t("protect.email.body"),
                done: item.done, action: onAddEmail)
        case .passkey:
            option(
                icon: .passkey, title: l10n.t("protect.passkey.title"),
                badge: nil, detail: l10n.t("protect.passkey.body"),
                done: item.done, action: onAddPasskey)
        case .phrase:
            option(
                icon: .key, title: l10n.t("protect.phrase.title"),
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

/// Which way "Save a recovery phrase" goes (ADR-0075 section 3, amended
/// 2026-11-01), pure. A rotation exists to outrun a copy of the phrase in an
/// email vault. A phrase that was never in one has no copy to outrun, and a
/// rotation there would only leave the account talking under a key its phrase
/// does not hold (ADR-0074 section 3.7).
enum SavePhraseRoute: Equatable {
    /// The words this device already holds: no key changes.
    case heldWords(String)
    /// The ADR-0074 key rotation, with new words.
    case rotation

    /// - Parameter heldPhrase: the phrase this device holds, when it is the
    ///   account's. A device that holds none has nothing to show, so it
    ///   rotates.
    static func decide(rotationNeeded: Bool, heldPhrase: String?) -> SavePhraseRoute {
        if !rotationNeeded, let heldPhrase { return .heldWords(heldPhrase) }
        return .rotation
    }
}

/// What the save-phrase screen shows, pure.
enum SavePhraseState: Equatable {
    /// `GET /account/phrase` is out: nothing tells the two routes apart yet.
    case checking
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
    /// No key changes: the words this device already holds. The account
    /// counts as protected only once the server was told, after the
    /// three-word check.
    case heldWords(String)

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

/// The flow behind ``SavePhraseScreen`` (UX 19.5), over injected ports.
@MainActor
final class SavePhraseFlow: ObservableObject {

    struct Ports {
        var accountIdB64: @MainActor () -> String?
        var sessionToken: @MainActor () -> String?
        var freshGrant: @MainActor () -> String?
        /// `GET /account/phrase`.
        var readStatus: (_ sessionToken: String) async throws -> AccountPhraseStatus
        /// The phrase this device holds, when it is the account's.
        var heldPhrase: @MainActor () -> String?
        /// This device is about to say the phrase was saved: the notice the
        /// server writes for it is its own act.
        var markOwnAction: @MainActor (_ accountIdB64: String) -> Void
        /// `POST /account/phrase/saved`.
        var markSaved: (_ sessionToken: String, _ grant: String) async throws -> Void
        /// The server took it: the account is phrase-protected from here.
        var phraseWasSaved: @MainActor (_ accountIdB64: String) -> Void
        /// `KeyRotationRunner.savePhrase()`.
        var rotate: @MainActor () async -> KeyRotationPlan.Result
        /// What a stored rotation asks for, read again.
        var rotationResume: @MainActor () -> KeyRotationPlan.Resume
        /// `KeyRotationRunner.resumeOwed()`.
        var finishRotation: @MainActor () async -> Void
        /// `KeyRotationRunner.wordsToShow()`.
        var rotationWords: @MainActor () -> String?
        /// The proof ran out: the block appears again.
        var proofExpired: @MainActor () -> Void
    }

    @Published private(set) var state: SavePhraseState = .checking
    /// The route, once the server answered. `nil` while unknown: no path is
    /// ever guessed.
    @Published private(set) var route: SavePhraseRoute?
    /// `POST /account/phrase/saved` is out.
    @Published private(set) var confirming = false
    /// The three words passed and the server was not told: the screen stays,
    /// with `Try again`.
    @Published private(set) var confirmFailed = false

    private let ports: Ports
    private var opened = false
    /// The rotation needs the session's runtime (the DeviceGroup, the chats,
    /// the reach keys), which is not up on the sign-up step. There, this is
    /// set: the route lands in the app and the rotation runs from Settings.
    private let rotationNeedsSession: (@MainActor () -> Void)?

    init(ports: Ports, rotationNeedsSession: (@MainActor () -> Void)? = nil) {
        self.ports = ports
        self.rotationNeedsSession = rotationNeedsSession
    }

    /// Whether this screen says the account gets new keys, and shows the
    /// rotation's progress line.
    var changesKeys: Bool { route == .rotation }

    /// Whether the main button asks for the proof first. Finding out the
    /// route again and finishing an owed publish do not.
    var primaryNeedsProof: Bool { route != nil && state != .publishOwed }

    /// The screen opened: a change that already went through is finished,
    /// never started again; otherwise the server says which route this is.
    func open() async {
        guard !opened else { return }
        opened = true
        switch ports.rotationResume() {
        case .showWords, .finishOwed, .commitLandedUnseen:
            await finishRotation()
        case .nothing, .removedNotRotated:
            await readRoute()
        }
    }

    /// The main button.
    func primary() async {
        if state == .publishOwed {
            await finishRotation()
            return
        }
        switch route {
        case nil: await readRoute()
        case .heldWords(let phrase): state = .heldWords(phrase)
        case .rotation: await rotate()
        }
    }

    /// The three words passed: tell the server. `true` when it took it; the
    /// caller then lands where the rotation lands. On a refusal that says the
    /// phrase needs a rotation after all (it was sealed in a vault since the
    /// route was read) the rotation runs from here, and this screen goes on
    /// with its states.
    func confirmHeldWords() async -> Bool {
        guard case .heldWords = state, !confirming else { return false }
        confirming = true
        confirmFailed = false
        defer { confirming = false }
        guard let accountIdB64 = ports.accountIdB64(), let token = ports.sessionToken() else {
            confirmFailed = true
            return false
        }
        guard let grant = ports.freshGrant() else {
            ports.proofExpired()
            return false
        }
        ports.markOwnAction(accountIdB64)
        do {
            try await ports.markSaved(token, grant)
        } catch {
            NSLog("[protect] the saved phrase was not recorded: %@", String(describing: type(of: error)))
            if PhraseSaveRefusal.needsRotation(error) {
                route = .rotation
                await rotate()
            } else if FreshAuthRefusal.isRequired(error) {
                // Not an error: the block appears, as anywhere a proof ran out.
                ports.proofExpired()
            } else {
                confirmFailed = true
            }
            return false
        }
        ports.phraseWasSaved(accountIdB64)
        return true
    }

    private func readRoute() async {
        state = .checking
        guard let token = ports.sessionToken() else {
            state = .failed
            return
        }
        do {
            let status = try await ports.readStatus(token)
            route = SavePhraseRoute.decide(
                rotationNeeded: status.rotationNeeded, heldPhrase: ports.heldPhrase())
            state = .idle
            if route == .rotation { rotationNeedsSession?() }
        } catch {
            NSLog("[protect] whether saving a phrase changes the keys could not be read")
            state = .failed
        }
    }

    private func rotate() async {
        if let rotationNeedsSession {
            state = .idle
            rotationNeedsSession()
            return
        }
        state = .working
        let result = await ports.rotate()
        if result == .freshAuthRequired { ports.proofExpired() }
        state = SavePhraseState.after(result)
    }

    /// Finish what a committed change still owes; show the words when
    /// nothing withholds them.
    private func finishRotation() async {
        state = .working
        await ports.finishRotation()
        if let phrase = ports.rotationWords() {
            state = .words(phrase)
        } else if ports.rotationResume() == .nothing {
            await readRoute()
        } else {
            state = .publishOwed
        }
    }
}

extension SavePhraseFlow.Ports {

    /// The production ports.
    @MainActor
    static func live(
        authRepository: AuthRepository, rotation: KeyRotationRunner, proof: FreshAuthProofModel
    ) -> SavePhraseFlow.Ports {
        SavePhraseFlow.Ports(
            accountIdB64: { [weak authRepository] in
                authRepository?.identitySecrets.map { base64Encode($0.accountId) }
            },
            sessionToken: { [weak authRepository] in authRepository?.identitySecrets?.sessionToken },
            freshGrant: { [weak authRepository] in authRepository?.liveFreshGrant() },
            readStatus: { try await AuthClient().accountPhraseStatus(sessionToken: $0) },
            heldPhrase: { [weak authRepository] in authRepository?.heldPhraseToSave() },
            markOwnAction: { [weak authRepository] accountIdB64 in
                authRepository?.protectionStore.markOwnAction(
                    .phraseSaved, atMs: Int64(Date().timeIntervalSince1970 * 1000), accountIdB64: accountIdB64)
            },
            markSaved: { token, grant in
                try await AuthClient().markPhraseSaved(sessionToken: token, freshGrant: grant)
            },
            phraseWasSaved: { [weak authRepository] accountIdB64 in
                authRepository?.notePhraseSaved(accountIdB64: accountIdB64)
            },
            rotate: { [weak rotation] in
                await rotation?.savePhrase() ?? .removedNotRotated(nil)
            },
            rotationResume: { [weak rotation] in
                rotation?.reload()
                return rotation?.resume ?? .nothing
            },
            finishRotation: { [weak rotation] in await rotation?.resumeOwed() },
            rotationWords: { [weak rotation] in rotation?.wordsToShow() },
            proofExpired: { [weak authRepository, weak proof] in
                authRepository?.dropFreshAuth()
                proof?.proofExpired()
            }
        )
    }
}

/// "Save a recovery phrase" (UX 19.5). Two routes behind one screen
/// (``SavePhraseRoute``): the words the device already holds, with the
/// three-word check and then the server told; or, for a phrase that was ever
/// in an email vault, the ADR-0074 key rotation with no device removed.
struct SavePhraseScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    let rotation: KeyRotationRunner
    /// The words were confirmed.
    let onDone: () -> Void

    @StateObject private var proof: FreshAuthProofModel
    @StateObject private var flow: SavePhraseFlow
    @ObservedObject private var l10n = Localization.shared

    /// - Parameter rotationNeedsSession: set on the sign-up step, where the
    ///   session's runtime is not up: called instead of rotating.
    init(
        authRepository: AuthRepository, center: AccountProtectionCenter, rotation: KeyRotationRunner,
        rotationNeedsSession: (@MainActor () -> Void)? = nil,
        onDone: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.center = center
        self.rotation = rotation
        self.onDone = onDone
        let proof = FreshAuthProofModel(surface: .savePhrase, repository: authRepository)
        self._proof = StateObject(wrappedValue: proof)
        self._flow = StateObject(wrappedValue: SavePhraseFlow(
            ports: .live(authRepository: authRepository, rotation: rotation, proof: proof),
            rotationNeedsSession: rotationNeedsSession))
    }

    private var working: Bool { flow.state == .working || proof.layout.working }

    var body: some View {
        // One container, so the screen opens once and closes once however
        // often the state swaps what is inside it.
        ZStack {
            switch flow.state {
            case .words(let phrase):
                guide(phrase, onDone: {
                    rotation.acknowledgePhrase()
                    Task { await center.refresh() }
                    onDone()
                })
            case .heldWords(let phrase):
                guide(
                    phrase,
                    doneTitle: flow.confirmFailed ? l10n.t("common.try_again") : nil,
                    doneBusy: flow.confirming || proof.layout.working,
                    confirmFooter: AnyView(heldWordsFooter),
                    onDone: confirmHeldWords)
            case .checking, .idle, .working, .failed, .publishOwed:
                explanation
            }
        }
        .task {
            await proof.opened()
            await flow.open()
        }
        .onDisappear { proof.closed() }
    }

    private func guide(
        _ phrase: String,
        doneTitle: String? = nil,
        doneBusy: Bool = false,
        confirmFooter: AnyView? = nil,
        onDone: @escaping () -> Void
    ) -> some View {
        RecoveryPhraseGuide(
            phrase: phrase,
            title: l10n.t("recovery_phrase.save.title"),
            intro: l10n.t("recovery_phrase.save.lede"),
            doneTitle: doneTitle,
            doneBusy: doneBusy,
            confirmFooter: confirmFooter,
            extra: { AuthHint(text: l10n.t("recovery_phrase.save.anyone_with_words")) },
            onDone: onDone
        )
    }

    /// Under the three words of the held-words route: the proof block when
    /// the proof ran out while the person wrote the words down, and the
    /// failure of the last call.
    @ViewBuilder private var heldWordsFooter: some View {
        // No submit: the return key must not pass the three-word check by.
        FreshAuthBlock(model: proof)
        if flow.confirmFailed {
            AuthHint(text: l10n.t("protect.phrase.save.failed"), isError: true)
        }
    }

    private var explanation: some View {
        AuthScreen {
            AuthTitle(
                title: l10n.t("protect.phrase.title"),
                subtitle: l10n.t("protect.phrase.save.body"))
            if flow.changesKeys {
                AuthHint(text: l10n.t("protect.phrase.save.effect"))
            }
            switch flow.state {
            case .failed:
                AuthHint(text: l10n.t("protect.phrase.save.failed"), isError: true)
            case .publishOwed:
                AuthHint(text: l10n.t("protect.phrase.save.finishing"), isError: true)
            case .checking, .idle, .working, .words, .heldWords:
                FreshAuthBlock(model: proof, onSubmit: save)
            }
            AuthPrimaryButton(
                title: l10n.t(
                    flow.state == .idle || flow.state == .checking
                        ? "protect.phrase.save.show" : "common.try_again"),
                busy: working,
                busyTitle: l10n.t(flow.changesKeys ? "protect.phrase.save.progress" : "common.checking"),
                disabled: flow.state == .checking || (flow.state == .idle && !proof.layout.ready),
                action: save
            )
        }
        .navigationTitle(l10n.t("protect.phrase.title"))
        .navigationBarTitleDisplayMode(.inline)
    }

    private func save() {
        guard !working, flow.state != .checking else { return }
        Task {
            if flow.primaryNeedsProof {
                guard await proof.ensure() else { return }
            }
            await flow.primary()
        }
    }

    /// The three words passed. Nothing counts as saved before the server
    /// took it; then the screen lands where the rotation lands.
    private func confirmHeldWords() {
        guard !flow.confirming, !proof.layout.working else { return }
        Task {
            guard await proof.ensure() else { return }
            if await flow.confirmHeldWords() {
                Task { await center.refresh() }
                onDone()
            }
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
        if let rows = center.rows {
            HStack {
                Label(l10n.t(rows.statusKey), icon: .lockShield)
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
                    title: l10n.t("recovery_email.title"), icon: .envelope,
                    value: l10n.t("recovery_email.add_again.title")
                ) { onOpen(.recoveryEmail(remove: false)) }
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
                AuthSecondaryButton(title: l10n.t("recovery_email.remove"), icon: .envelope) {
                    onOpen(.recoveryEmail(remove: true))
                }
            }
            if actions.contains(.savePhrase) {
                AuthSecondaryButton(title: l10n.t("protect.phrase.title"), icon: .key) {
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
