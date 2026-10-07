// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The sign-up form (UX spec section 5.6): username with the live
/// availability hint, name, password, confirm, Create account. Pushed
/// from ``WelcomeScreen``.
///
/// The steps that follow a successful sign-up (the recovery phrase, its
/// confirmation, the passkey offer) are NOT rendered here: the repository
/// lands the session as part of sign-up, which replaces the anonymous
/// stack, so the phrase is handed up through
/// ``SignupViewModel/onPhraseReady`` and the app root shows
/// ``SignupPhraseScreen`` over the session until the user is through.
public struct SignupScreen: View {

    @ObservedObject public var viewModel: SignupViewModel
    public var onNavigateToLogin: () -> Void
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: SignupViewModel,
        onNavigateToLogin: @escaping () -> Void = {}
    ) {
        self.viewModel = viewModel
        self.onNavigateToLogin = onNavigateToLogin
    }

    public var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("auth.signup.title"), subtitle: l10n.t("auth.signup.lede"))

            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: l10n.t("auth.username.label"),
                    text: $viewModel.username,
                    contentType: .username,
                    disabled: viewModel.busy
                )
                // ADR-0048 section 4: the reason, as the user types.
                if let hint = viewModel.usernameHint {
                    AuthHint(text: hint, isError: true)
                } else {
                    AuthHint(text: l10n.t("auth.signup.username.hint"))
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                AuthTextField(
                    label: l10n.t("auth.signup.name.label"),
                    placeholder: viewModel.username,
                    text: $viewModel.displayName,
                    contentType: .name,
                    capitalization: .words,
                    autocorrect: false,
                    disabled: viewModel.busy
                )
                if let nameError = viewModel.displayNameError {
                    AuthHint(text: nameError, isError: true)
                } else {
                    AuthHint(text: l10n.t("auth.signup.name.hint"))
                }
            }

            AuthSecureField(
                label: l10n.t("auth.password.label"),
                text: $viewModel.password,
                contentType: .newPassword,
                disabled: viewModel.busy
            )
            AuthHint(text: l10n.t(
                "auth.password.hint", counts: ["count": UInt64(SignupViewModel.minPasswordLength)]))

            AuthSecureField(
                label: l10n.t("auth.password.confirm.label"),
                text: $viewModel.confirm,
                contentType: .newPassword,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.onSubmit
            )

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            // ADR-0054 S4 / `docs/ux/anti-abuse.md` section 1: the one heavy
            // wait in the app. The button becomes a disabled working row, one
            // line below it, and `Cancel` back to the filled-in form.
            AuthPrimaryButton(
                title: l10n.t(viewModel.rateLimited ? "common.try_again" : "auth.signup.submit"),
                busy: viewModel.busy,
                busyTitle: SignupViewModel.workingTitle,
                action: viewModel.rateLimited ? viewModel.onTryAgain : viewModel.onSubmit
            )
            if viewModel.busy {
                AuthHint(text: SignupViewModel.workingNote)
                if viewModel.cancellable {
                    AuthSecondaryButton(title: l10n.t("common.cancel"), action: viewModel.onCancel)
                }
            }

            AuthLinkRow(
                message: "auth.signup.have_account",
                title: l10n.t("auth.login.submit"),
                disabled: viewModel.busy,
                action: onNavigateToLogin
            )
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        // Leaving the screen while working cancels the work.
        .onDisappear { viewModel.onCancel() }
    }
}

/// The guided steps after sign-up, shown by the app root over the freshly
/// landed session; `onDone` lets the session through.
///
///   1. The 12 words in a grid, Copy, the explanation.
///   2. The three-word confirmation.
///   3. "Add a passkey" / "Not now" (accounts with a username), followed by
///      the opt-in choice of letting that passkey restore keys when the
///      platform can protect keys with it.
struct SignupPhraseScreen: View {
    let phrase: String
    @ObservedObject var authRepository: AuthRepository
    let onDone: () -> Void

    private enum Step: Equatable {
        case phrase
        case passkey
        case keysChoice(AddedPasskey)
        /// The result line of the passkey step, then Done.
        case result(String)
    }

    @State private var step: Step = .phrase
    @State private var busy = false
    @State private var error: UiText?
    /// UX 17.1 — the proof block of the passkey step, the same one Settings
    /// > Passkeys draws. Absent while the proof of the signup finish is
    /// live; on screen once the onboarding sat open past its ten minutes.
    @StateObject private var proof: FreshAuthProofModel
    @ObservedObject private var l10n = Localization.shared

    init(phrase: String, authRepository: AuthRepository, onDone: @escaping () -> Void) {
        self.phrase = phrase
        self.authRepository = authRepository
        self.onDone = onDone
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .addPasskey, repository: authRepository))
    }

    /// The session account, when it has a username: a guest account is not
    /// offered a passkey here.
    private var namedAccount: SessionAccount? {
        guard case .session(let account) = authRepository.state, !account.username.isEmpty else {
            return nil
        }
        return account
    }

    private var totalSteps: Int { namedAccount == nil ? 2 : 3 }

    var body: some View {
        switch step {
        case .phrase:
            RecoveryPhraseGuide(
                phrase: phrase,
                title: l10n.t("recovery_phrase.save.title"),
                intro: l10n.t("recovery_phrase.save.lede"),
                totalSteps: totalSteps,
                extra: { EmptyView() },
                onDone: {
                    if namedAccount == nil { onDone() } else { step = .passkey }
                }
            )
        case .passkey:
            AuthScreen {
                PhraseStepLabel(current: 3, total: 3)
                AuthTitle(
                    title: l10n.t("auth.signup.passkey.title"),
                    subtitle: l10n.t("auth.signup.passkey.lede"))
                FreshAuthBlock(model: proof, onSubmit: addPasskey)
                if let error {
                    AuthHint(text: error.text, isError: true)
                }
                AuthPrimaryButton(
                    title: l10n.t("auth.signup.passkey.add"),
                    busy: busy,
                    busyTitle: l10n.t(
                        SignupPasskeyStep.busyTitleKey(checkingProof: proof.layout.working)),
                    disabled: !proof.layout.ready,
                    action: addPasskey
                )
                AuthSecondaryButton(title: l10n.t("common.not_now"), disabled: busy, action: onDone)
            }
            .task { await proof.opened() }
            .onDisappear { proof.closed() }
        case .keysChoice(let added):
            AuthScreen {
                PhraseStepLabel(current: 3, total: 3)
                PasskeyKeysChoiceView(
                    credentialIdB64: added.credentialIdB64,
                    nickname: added.nickname,
                    prfOutput: added.prfOutput,
                    enrollment: .live(authRepository: authRepository, client: WebauthnClient()),
                    onDone: { result in
                        if let key = result.messageKey {
                            step = .result(key)
                        } else {
                            onDone()
                        }
                    }
                )
            }
        case .result(let messageKey):
            AuthScreen {
                PhraseStepLabel(current: 3, total: 3)
                AuthTitle(title: l10n.t("auth.signup.passkey.title"))
                AuthHint(text: l10n.t(messageKey))
                AuthPrimaryButton(title: l10n.t("common.done"), action: onDone)
            }
        }
    }

    private func addPasskey() {
        guard !busy, proof.layout.ready else { return }
        guard let account = namedAccount, let secrets = authRepository.identitySecrets else {
            error = .key("common.error.session_not_ready")
            return
        }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            // The fresh-auth proof first: the proof of the signup finish is
            // live here, and nothing is asked. Past its ten minutes the
            // block is on screen and its password is checked now; a failure
            // stays in the block.
            guard await proof.ensure(), let freshGrant = authRepository.liveFreshGrant() else {
                if proof.proofLive { proof.proofExpired() }
                return
            }
            let client = WebauthnClient()
            // The proof may have rotated the session.
            client.setSessionToken(authRepository.identitySecrets?.sessionToken ?? secrets.sessionToken)
            do {
                let added = try await PasskeyAdder.add(
                    client: client,
                    registration: WebauthnRegistration(),
                    userName: account.username,
                    nickname: l10n.t("auth.signup.passkey.default_name"),
                    account: (accountIdBytes: secrets.accountId, accountIdB64: account.accountIdB64),
                    freshGrant: freshGrant,
                    presentationAnchor: currentPresentationAnchor()
                )
                switch added.outcome {
                case .offerKeysChoice: step = .keysChoice(added)
                case .signInOnly: step = .result("settings.passkeys.added_no_keys")
                }
            } catch {
                switch SignupPasskeyStep.failure(error) {
                case .stay:
                    break
                case .proofNeeded:
                    // The proof ran out between the two calls: the block
                    // appears, with no error styling. Nothing was added.
                    authRepository.dropFreshAuth()
                    proof.proofExpired()
                case .error:
                    NSLog("[signup] WARN adding a passkey failed")
                    self.error = .key("auth.signup.passkey.error")
                }
            }
        }
    }
}

/// The pure decisions of the onboarding's passkey step.
enum SignupPasskeyStep {

    enum Failure: Equatable {
        /// The passkey sheet was dismissed: not an error, the step stays.
        case stay
        /// `403 fresh-auth-required`: the proof block appears.
        case proofNeeded
        case error
    }

    /// What a failed add leaves on the step.
    static func failure(_ error: Error) -> Failure {
        if AuthErrorCopy.isPasskeyCancelled(error) { return .stay }
        if FreshAuthRefusal.isRequired(error) { return .proofNeeded }
        return .error
    }

    /// The button's working word: the password check of the proof block,
    /// then the platform's passkey sheet.
    static func busyTitleKey(checkingProof: Bool) -> String {
        checkingProof ? "common.checking" : "recovery.passkey.waiting"
    }
}
