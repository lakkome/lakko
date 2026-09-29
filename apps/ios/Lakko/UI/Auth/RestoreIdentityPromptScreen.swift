// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0017 fresh-device state (``AuthState/needsRecoveryPhrase(_:)``):
/// the user signed in on a device that holds no copy of their identity, so
/// the messages cannot be read yet. Two ways forward, the recovery phrase
/// or a device that is still signed in, and one escape hatch for a user
/// who has neither. The verification itself is
/// ``AuthRepository/provideRecoveryPhrase(mnemonicPhrase:)``, unchanged.
public struct RestoreIdentityPromptScreen: View {

    @ObservedObject public var viewModel: RestoreIdentityPromptViewModel

    private enum Method: Hashable { case phrase, device }

    @State private var method: Method = .phrase
    @State private var showingStartOver = false
    @ObservedObject private var l10n = Localization.shared

    public init(viewModel: RestoreIdentityPromptViewModel) {
        self.viewModel = viewModel
    }

    public var body: some View {
        AuthScreen {
            // No three-word recovery on this client (ADR-0049 is web-only).
            AuthTitle(
                title: l10n.t("restore_identity.title"),
                subtitle: l10n.t(
                    "restore_identity.lede_no_words", texts: ["username": "@\(viewModel.username)"]))

            Picker(l10n.t("restore_identity.title"), selection: $method) {
                Text(l10n.t("recovery.method.phrase.title")).tag(Method.phrase)
                Text(l10n.t("recovery.method.device.title")).tag(Method.device)
            }
            .pickerStyle(.segmented)
            .disabled(viewModel.busy)

            switch method {
            case .phrase:
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

            case .device:
                if let accountIdBytes = viewModel.accountIdBytes,
                   let handle = ownLoginHandle(username: viewModel.username) {
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
                } else if viewModel.accountIdBytes != nil {
                    // ADR-0056 — the pairing code names the account by its
                    // username; an account without one cannot be named.
                    AuthHint(text: PairingScanFailure.needsUsernameCopy, isError: true)
                } else {
                    AuthHint(text: l10n.t("restore_identity.error.bad_saved_account"), isError: true)
                }
            }

            // ADR-0017 lost-everything fallback: not a primary action, and
            // spelled out before the button.
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
                        Text(l10n.t("restore_identity.lost.warning"))
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        Button(action: viewModel.startFresh) {
                            Text(l10n.t(viewModel.busy
                                ? "restore_identity.lost.creating"
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

            AuthLinkRow(title: l10n.t("restore_identity.log_out"), disabled: viewModel.busy, action: viewModel.onSignOut)
        }
        .onChange(of: method) { _, _ in
            // A stale error from the other method should not linger.
            viewModel.clearError()
        }
    }
}
