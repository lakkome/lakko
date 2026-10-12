// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// "Restore your keys" (``AuthState/needsRecoveryPhrase(_:)``): the person
/// is signed in on a device that does not hold the account's keys, so the
/// messages cannot be read yet. A list of routes (another device, a passkey
/// that carries keys, the recovery phrase), each opening its form, and one
/// escape hatch for a person who has none of them. When a passkey sign-in
/// brought no keys, the first line says so.
public struct RestoreIdentityPromptScreen: View {

    @StateObject private var viewModel: RestoreIdentityPromptViewModel
    @ObservedObject private var authRepository: AuthRepository

    /// `nil` shows the list; a method shows that method's form.
    @State private var method: RestoreIdentityPromptViewModel.Method?
    @State private var showingStartOver = false
    /// The wait a reset's `429` asked for (UX 15.1: the shared line with its
    /// countdown, the panel stays open).
    @StateObject private var resetWait = RetryCountdown()
    @ObservedObject private var l10n = Localization.shared

    public init(
        authRepository: AuthRepository,
        viewModel: @autoclosure @escaping () -> RestoreIdentityPromptViewModel
    ) {
        self.authRepository = authRepository
        self._viewModel = StateObject(wrappedValue: viewModel())
    }

    public var body: some View {
        AuthScreen {
            if viewModel.resetting {
                // UX 15.1 — after the tap: "Resetting..." with no cancel; the
                // server step is one operation and cannot be half done.
                AuthTitle(title: l10n.t("restore_identity.lost.sub"))
                HStack(spacing: 10) {
                    ProgressView()
                    Text(l10n.t("restore_identity.reset.resetting"))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                .accessibilityElement(children: .combine)
            } else {
                if let method {
                    methodForm(method)
                } else {
                    list
                }

                startOver

                AuthLinkRow(title: l10n.t("restore_identity.log_out"), disabled: viewModel.busy, action: viewModel.onSignOut)
            }
        }
        .task { await viewModel.loadPasskeysWithKeys() }
        .task { await viewModel.loadProtection() }
        .onChange(of: method) { _, _ in
            // A stale error from another method should not linger.
            viewModel.clearError()
            // Nothing typed for the email code outlives its form.
            if method != .email { viewModel.email.backToStart() }
        }
        .onChange(of: showingStartOver) { _, showing in
            // UX 15.1a — the password is never kept after the panel closes.
            if showing { viewModel.resetPanelOpened() } else { viewModel.resetPanelClosed() }
        }
        .onDisappear { viewModel.resetPanelClosed() }
        .onChange(of: viewModel.resetRetryAfterSecs) { _, secs in
            if let secs { resetWait.start(forSecs: secs) }
        }
    }

    // MARK: - List

    @ViewBuilder
    private var list: some View {
        AuthTitle(title: l10n.t("restore_identity.title"))

        if let notice = authRepository.restoreNotice {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "info.circle")
                    .foregroundStyle(Color.lakkoAccent)
                Text(l10n.t(notice.catalogKey))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(12)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.card)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
        }

        Text(l10n.t("restore_identity.lede", texts: ["username": displayedAccount]))
            .font(.subheadline)
            .foregroundStyle(Color.lakkoTextMuted)

        RecoveryMethodList(
            rows: viewModel.methods.map { row(for: $0) },
            disabled: viewModel.busy
        ) { id in
            method = RestoreIdentityPromptViewModel.Method(rawValue: id)
        }
    }

    private var displayedAccount: String {
        viewModel.username.isEmpty ? "" : "@\(viewModel.username)"
    }

    private func row(for method: RestoreIdentityPromptViewModel.Method) -> RecoveryMethodRow {
        switch method {
        case .email:
            return RecoveryMethodRow(
                id: method.rawValue,
                systemImage: "envelope",
                title: l10n.t("recovery.email.row.title"),
                detail: l10n.t("recovery.email.row.body"))
        case .device:
            return RecoveryMethodRow(
                id: method.rawValue,
                systemImage: "iphone.gen3.radiowaves.left.and.right",
                title: l10n.t("recovery.method.device.title"),
                detail: l10n.t("recovery.method.device.sub"))
        case .passkey:
            return RecoveryMethodRow(
                id: method.rawValue,
                systemImage: "person.badge.key",
                title: l10n.t("recovery.method.passkey.title"),
                detail: l10n.t("restore_identity.method.passkey.sub"))
        case .phrase:
            return RecoveryMethodRow(
                id: method.rawValue,
                systemImage: "key",
                title: l10n.t("recovery.method.phrase.title"),
                detail: l10n.t("recovery.method.phrase.restore_sub"))
        }
    }

    // MARK: - Forms

    @ViewBuilder
    private func methodForm(_ picked: RestoreIdentityPromptViewModel.Method) -> some View {
        Button {
            method = nil
        } label: {
            Label(l10n.t("recovery.methods.back"), systemImage: "chevron.backward")
                .font(.footnote.weight(.medium))
                .foregroundStyle(Color.lakkoAccent)
        }
        .buttonStyle(.plain)
        .disabled(viewModel.busy)

        AuthTitle(title: row(for: picked).title)

        switch picked {
        case .email:
            EmailRecoverSection(model: viewModel.email, onBackToList: {
                viewModel.email.backToStart()
                method = nil
            })
        case .device:
            deviceForm
        case .passkey:
            AuthHint(text: l10n.t("recovery.passkey.intro"))
            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            AuthPrimaryButton(
                title: l10n.t("recovery.passkey.submit"),
                busy: viewModel.busy,
                busyTitle: l10n.t("recovery.passkey.waiting")
            ) {
                Task {
                    if await viewModel.restoreWithPasskey() { method = nil }
                }
            }
        case .phrase:
            AuthHint(text: l10n.t("restore_identity.phrase_intro"))
            PhraseEntryGrid(phrase: $viewModel.phrase, disabled: viewModel.busy)

            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }

            AuthPrimaryButton(
                title: l10n.t("restore_identity.submit"),
                busy: viewModel.busy,
                busyTitle: l10n.t("restore_identity.submitting"),
                action: viewModel.onSubmit
            )
        }
    }

    @ViewBuilder
    private var deviceForm: some View {
        if viewModel.username.isEmpty, viewModel.confirmedTypedUsername == nil {
            // The sign-in named no account (a passkey): the pairing code
            // names the account by its username, so ask for it.
            AuthTextField(
                label: l10n.t("auth.username.label"),
                text: $viewModel.typedUsername,
                contentType: .username,
                disabled: viewModel.busy,
                submitLabel: .go,
                onSubmit: viewModel.confirmTypedUsername
            )
            UsernameRuleHint(input: viewModel.typedUsername)
            if let error = viewModel.error {
                AuthHint(text: error, isError: true)
            }
            AuthPrimaryButton(title: l10n.t("common.continue"), action: viewModel.confirmTypedUsername)
        }
        if let accountIdBytes = viewModel.accountIdBytes,
           let handle = viewModel.pairingLoginHandle {
            RestoreViaDevicePanel(
                target: .account(
                    accountIdBytes: accountIdBytes,
                    accountIdB64: viewModel.accountIdB64,
                    loginHandle: handle
                ),
                onRecovered: { outcome in
                    try await viewModel.completeFromPairing(outcome: outcome)
                }
            )
            .padding(16)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
            .overlay(
                RoundedRectangle(cornerRadius: LakkoRadius.card)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
        } else if viewModel.accountIdBytes == nil {
            AuthHint(text: l10n.t("restore_identity.error.bad_saved_account"), isError: true)
        } else if !viewModel.username.isEmpty {
            // The pairing code names the account by its username; this
            // account's name derives no handle.
            AuthHint(text: PairingScanFailure.needsUsernameCopy, isError: true)
        }
    }

    // MARK: - Start over

    /// UX 15.1 — the "What it does" bullets, in order.
    private let resetEffects = [
        "restore_identity.reset.effect_devices",
        "restore_identity.reset.effect_passkeys",
        "restore_identity.reset.effect_contacts",
        "restore_identity.reset.effect_messages",
        "restore_identity.reset.effect_password",
    ]

    /// UX 17.2 — "The account keys cannot be reset without your password or
    /// a passkey": text only (each option is a row of this screen, one tap
    /// back through Cancel), no disabled reset button, no promise of a later
    /// way.
    private var cannotResetPanel: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(l10n.t("proof.reset.cannot.title"))
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.lakkoText)
            Text(l10n.t("proof.reset.cannot.body"))
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
            Button(l10n.t("proof.know_password_link")) {
                viewModel.resetKnowsPassword(true)
            }
            .buttonStyle(.plain)
            .font(.body.weight(.medium))
            .foregroundStyle(Color.lakkoAccent)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
        }
    }

    /// The lost-everything fallback: not a primary action, and spelled out
    /// before the button.
    private var startOver: some View {
        VStack(alignment: .leading, spacing: 12) {
            Button {
                withAnimation { showingStartOver.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Text(l10n.t("restore_identity.lost.summary"))
                        .font(.footnote.weight(.medium))
                    Image(systemName: showingStartOver ? "chevron.up" : "chevron.down")
                        .font(.caption.weight(.semibold))
                }
                .foregroundStyle(Color.lakkoAccent)
            }
            .buttonStyle(.plain)
            .disabled(viewModel.busy)

            if showingStartOver {
                VStack(alignment: .leading, spacing: 12) {
                    // UX 15.1 — the warning: what a reset does, unconditional
                    // (the device does not know how many other devices or
                    // passkeys the account has, and the number changes
                    // nothing about the decision).
                    Text(l10n.t("restore_identity.lost.sub"))
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                    Text(l10n.t("restore_identity.reset.lede"))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                    Text(l10n.t("restore_identity.reset.what_it_does"))
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Color.lakkoText)
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(resetEffects, id: \.self) { key in
                            HStack(alignment: .top, spacing: 8) {
                                Text("\u{2022}")
                                Text(l10n.t(key))
                            }
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                        }
                    }
                    if viewModel.resetForm == .cannotReset {
                        // UX 17.2 — the panel replaces the block, the typed
                        // word and the button; the warning above stays.
                        cannotResetPanel
                    } else {
                        // UX 17.2 — the proof block (absent while a proof is
                        // live), then the typed word, then the button.
                        FreshAuthBlock(model: viewModel.resetProof, onSubmit: viewModel.startFresh)
                        if viewModel.showsResetNoPasswordLink {
                            Button(l10n.t("proof.no_password_link")) {
                                viewModel.resetKnowsPassword(false)
                            }
                            .buttonStyle(.plain)
                            .font(.footnote.weight(.medium))
                            .foregroundStyle(Color.lakkoAccent)
                            .frame(minHeight: 44, alignment: .leading)
                            .disabled(viewModel.busy)
                        }
                        AuthTextField(
                            label: l10n.t(
                                "restore_identity.reset.confirm_label",
                                texts: ["word": KeyResetConfirmation.word]),
                            text: $viewModel.resetConfirmation,
                            capitalization: .characters,
                            autocorrect: false,
                            disabled: viewModel.busy,
                            submitLabel: .go,
                            onSubmit: viewModel.startFresh
                        )
                        if method == nil, let error = viewModel.error {
                            AuthHint(text: error, isError: true)
                        }
                        WaitNotice(countdown: resetWait)
                        Button(action: viewModel.startFresh) {
                            Text(l10n.t(viewModel.busy
                                ? "restore_identity.reset.resetting"
                                : "restore_identity.lost.create"))
                                .font(.body.weight(.medium))
                                .foregroundStyle(Color.lakkoDanger)
                                .frame(maxWidth: .infinity, minHeight: 44)
                                .overlay(
                                    RoundedRectangle(cornerRadius: LakkoRadius.control)
                                        .stroke(Color.lakkoDanger, lineWidth: 1)
                                )
                        }
                        .buttonStyle(.plain)
                        .disabled(viewModel.busy || !viewModel.canReset || resetWait.waiting)
                        .opacity(viewModel.canReset && !resetWait.waiting ? 1 : 0.5)
                    }
                    Button(l10n.t("common.cancel")) {
                        viewModel.resetPanelClosed()
                        withAnimation { showingStartOver = false }
                    }
                    .buttonStyle(.plain)
                    .font(.body.weight(.medium))
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .disabled(viewModel.busy)
                }
                .padding(16)
                .background(Color.lakkoSurface)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
                .overlay(
                    RoundedRectangle(cornerRadius: LakkoRadius.card)
                        .stroke(Color.lakkoBorder, lineWidth: 1)
                )
            }
        }
        .padding(.top, 8)
    }
}
