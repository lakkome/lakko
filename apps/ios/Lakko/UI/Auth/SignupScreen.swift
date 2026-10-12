// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The sign-up form (UX spec section 5.6): username with the live
/// availability hint, name, password, confirm, Create account. Pushed
/// from ``WelcomeScreen``.
///
/// The step that follows a successful sign-up ("Protect your account") is
/// NOT rendered here: the repository lands the session
/// as part of sign-up, which replaces the anonymous stack, so the app root
/// is told through ``SignupViewModel/onSignedUp`` and shows
/// ``SignupProtectScreen`` over the session until the user is through.
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

/// The step after sign-up, shown by the app root over the freshly landed
/// session; `onDone` lets the session through (UX 19.1a).
///
/// "Protect your account": `Add an email`, the passkey row, `Save a
/// recovery phrase`. The recovery phrase is NOT shown here (ADR-0067 D2):
/// the device keeps it unseen, and saving a phrase gives the account new
/// keys and new words. The email and the passkey return to the step with
/// their row marked done and `Continue` under the rows; saving a phrase
/// lands in the app directly. The passkey row runs the existing passkey
/// creation (with its "restore keys with this passkey" choice) under
/// sign-up's fresh grant.
struct SignupProtectScreen: View {
    @ObservedObject var authRepository: AuthRepository
    @ObservedObject var center: AccountProtectionCenter
    let rotation: KeyRotationRunner
    let onDone: () -> Void

    private enum Step: Equatable {
        case protect
        case email
        case phrase
        case passkey
        case keysChoice(AddedPasskey)
        /// The result line of the passkey step, then back to the step.
        case result(String)
    }

    @State private var step: Step = .protect
    @State private var busy = false
    @State private var error: UiText?
    /// UX 17.1 — the proof block of the passkey step, the same one Settings
    /// > Passkeys draws. Absent while the proof of the signup finish is
    /// live; on screen once the step sat open past its ten minutes.
    @StateObject private var proof: FreshAuthProofModel
    @ObservedObject private var l10n = Localization.shared

    init(
        authRepository: AuthRepository,
        center: AccountProtectionCenter,
        rotation: KeyRotationRunner,
        onDone: @escaping () -> Void
    ) {
        self.authRepository = authRepository
        self.center = center
        self.rotation = rotation
        self.onDone = onDone
        self._proof = StateObject(
            wrappedValue: FreshAuthProofModel(surface: .addPasskey, repository: authRepository))
    }

    private var account: SessionAccount? {
        guard case .session(let account) = authRepository.state else { return nil }
        return account
    }

    /// Back to the step with the passkey row marked done.
    private func passkeyDone() {
        center.passkeyAdded()
        step = .protect
    }

    var body: some View {
        switch step {
        case .protect:
            ProtectAccountScreen(
                center: center,
                hasUsername: account?.username.isEmpty == false,
                onAddEmail: { step = .email },
                onAddPasskey: {
                    error = nil
                    step = .passkey
                },
                onSavePhrase: { step = .phrase },
                onLeave: onDone
            )
        case .email:
            // Sign-up counts as the proof: nothing more is asked here.
            VStack(spacing: 0) {
                RecoveryEmailScreen(
                    authRepository: authRepository, center: center, onDone: { step = .protect })
                AuthLinkRow(title: l10n.t("common.back"), action: { step = .protect })
                    .padding(.bottom, 16)
            }
            .background(Color.lakkoBackground.ignoresSafeArea())
        case .phrase:
            VStack(spacing: 0) {
                SavePhraseScreen(
                    authRepository: authRepository, center: center, rotation: rotation, onDone: onDone)
                AuthLinkRow(title: l10n.t("common.back"), action: { step = .protect })
                    .padding(.bottom, 16)
            }
            .background(Color.lakkoBackground.ignoresSafeArea())
        case .passkey:
            AuthScreen {
                AuthTitle(
                    title: l10n.t("protect.passkey.title"),
                    subtitle: l10n.t("protect.passkey.body"))
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
                AuthLinkRow(title: l10n.t("common.back"), disabled: busy, action: { step = .protect })
            }
            .task { await proof.opened() }
            .onDisappear { proof.closed() }
        case .keysChoice(let added):
            AuthScreen {
                PasskeyKeysChoiceView(
                    credentialIdB64: added.credentialIdB64,
                    nickname: added.nickname,
                    prfOutput: added.prfOutput,
                    enrollment: .live(authRepository: authRepository, client: WebauthnClient()),
                    onDone: { result in
                        if let key = result.messageKey {
                            step = .result(key)
                        } else {
                            passkeyDone()
                        }
                    }
                )
            }
        case .result(let messageKey):
            AuthScreen {
                AuthTitle(title: l10n.t("protect.passkey.title"))
                AuthHint(text: l10n.t(messageKey))
                AuthPrimaryButton(title: l10n.t("protect.continue"), action: passkeyDone)
            }
        }
    }

    private func addPasskey() {
        guard !busy, proof.layout.ready else { return }
        guard let account, let secrets = authRepository.identitySecrets else {
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

/// The pure decisions of the passkey row's sub-step.
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
