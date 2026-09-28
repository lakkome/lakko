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

    public init(viewModel: RestoreIdentityPromptViewModel) {
        self.viewModel = viewModel
    }

    public var body: some View {
        AuthScreen {
            AuthTitle(
                title: "Restore your account on this device",
                subtitle: "You are signed in as @\(viewModel.username). To read your messages here, enter your recovery phrase or confirm from a device that is still signed in.")

            Picker("Restore with", selection: $method) {
                Text("Recovery phrase").tag(Method.phrase)
                Text("Another device").tag(Method.device)
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
                    title: "Restore",
                    busy: viewModel.busy,
                    busyTitle: "Restoring...",
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
                    AuthHint(
                        text: "Something is wrong with the account saved on this device. Log out and log in again, or use your recovery phrase.",
                        isError: true)
                }
            }

            // ADR-0017 lost-everything fallback: not a primary action, and
            // spelled out before the button.
            VStack(alignment: .leading, spacing: 12) {
                Button {
                    withAnimation { showingStartOver.toggle() }
                } label: {
                    HStack(spacing: 6) {
                        Text("I don't have either")
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
                        Text("Start over with a new identity")
                            .font(.body.weight(.medium))
                            .foregroundStyle(Color.lakkoText)
                        Text(
                            "Your messages on the lost device stay out of reach, your contacts will be asked to verify you again, and you get a new recovery phrase to save. Only do this if nothing else works."
                        )
                        .font(.subheadline)
                        .foregroundStyle(Color.lakkoTextMuted)
                        Button(action: viewModel.startFresh) {
                            Text("Start over")
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

            AuthLinkRow(title: "Log out", disabled: viewModel.busy, action: viewModel.onSignOut)
        }
        .onChange(of: method) { _, _ in
            // A stale error from the other method should not linger.
            viewModel.clearError()
        }
    }
}
