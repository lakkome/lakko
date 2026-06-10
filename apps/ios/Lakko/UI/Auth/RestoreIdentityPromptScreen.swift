// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0017 fresh-device fail-safe screen.
///
/// Mirror of `apps/web/src/auth/RestoreIdentityPrompt.tsx`.
///
/// Rendered whenever the auth state is
/// ``AuthState/needsRecoveryPhrase(_:)``: the user authenticated
/// successfully (password or passkey ACCESS) on a device that has NO
/// cached AccountIdentity (phrase) seed, so the canonical identity
/// cannot be rebuilt. Per ADR-0017 we MUST NOT derive the identity
/// from the OPAQUE export key (the wrong identity now that the
/// identity is phrase-seeded) nor silently mint a fresh one (which
/// would break every contact's pin). Instead the user supplies their
/// recovery phrase; ``AuthRepository/provideRecoveryPhrase(mnemonicPhrase:)``
/// derives the BIP39 seed, verifies it against the published bundle
/// (fail-closed on mismatch / fetch failure), caches it, and
/// completes the session.
///
/// Pairing an existing device over the E2EE channel (ADR-0018) is the
/// other acquisition route and is surfaced as a hint here, but the
/// phrase-entry path is what this screen wires.
public struct RestoreIdentityPromptScreen: View {

    @ObservedObject public var viewModel: RestoreIdentityPromptViewModel

    /// ADR-0018 factor (a): phrase entry OR restore from another
    /// device. Local UI-only toggle (no persisted state) — mirror of
    /// the Android `RestoreIdentityPrompt` FilterChip pair.
    @State private var viaDevice: Bool = false

    public init(viewModel: RestoreIdentityPromptViewModel) {
        self.viewModel = viewModel
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Restore your secure identity")
                    .font(.largeTitle)
                    .fontWeight(.semibold)

                Text(
                    "New device — enter your recovery phrase or pair an " +
                    "existing device to restore your secure identity for " +
                    "@\(viewModel.username). Your password got you in, but " +
                    "your long-term identity (the key your contacts trust) " +
                    "lives in your 24-word recovery phrase."
                )
                .font(.body)
                .foregroundStyle(.secondary)

                Picker("Restore mode", selection: $viaDevice) {
                    Text("Recovery phrase").tag(false)
                    Text("Another device").tag(true)
                }
                .pickerStyle(.segmented)
                .disabled(viewModel.busy)

                if viaDevice {
                    viaDevicePanel
                } else {
                    Text("Recovery phrase (24 words)")
                        .font(.headline)

                    phraseEditor

                    if let error = viewModel.error {
                        Text(error)
                            .font(.body)
                            .foregroundStyle(.red)
                    }

                    Button(action: viewModel.onSubmit) {
                        HStack {
                            if viewModel.busy {
                                ProgressView()
                                    .controlSize(.small)
                                    .progressViewStyle(.circular)
                                    .tint(.white)
                                Text("Restoring…")
                            } else {
                                Text("Restore identity")
                            }
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .disabled(viewModel.busy)

                    Text(
                        "Don't have your phrase? Switch to \"Another " +
                        "device\" above to carry your identity across " +
                        "from a signed-in device, or sign out and start " +
                        "a new account."
                    )
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                }

                HStack {
                    Spacer()
                    Button(action: viewModel.onSignOut) {
                        Text("Sign out")
                            .font(.footnote)
                    }
                    .disabled(viewModel.busy)
                    Spacer()
                }
            }
            .padding(24)
        }
    }

    /// ADR-0018 factor (a), path (a) — the QR-join flow with the
    /// recovery payload opted in; completion routes through
    /// ``RestoreIdentityPromptViewModel/completeFromPairing(outcome:)``.
    @ViewBuilder
    private var viaDevicePanel: some View {
        if let accountIdBytes = viewModel.accountIdBytes {
            RestoreViaDevicePanel(
                accountIdBytes: accountIdBytes,
                accountIdB64: viewModel.accountIdB64,
                onRecovered: { outcome in
                    try await viewModel.completeFromPairing(outcome: outcome)
                }
            )
        } else {
            Text(
                "The stored account id is malformed. Sign out and sign " +
                "in again, or restore with your recovery phrase."
            )
            .font(.footnote)
            .foregroundStyle(.red)
        }
    }

    /// Multi-line phrase editor with a manual placeholder overlay —
    /// same shape `RecoverScreen` uses. Autocorrect / autocapitalise
    /// are off: autocorrect on a wordlist phrase would helpfully
    /// rewrite valid BIP39 words into adjacent dictionary entries.
    private var phraseEditor: some View {
        ZStack(alignment: .topLeading) {
            if viewModel.phrase.isEmpty {
                Text("abandon abandon abandon … art")
                    .font(.system(.body, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
                    .allowsHitTesting(false)
            }
            TextEditor(text: $viewModel.phrase)
                .font(.system(.body, design: .monospaced))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .scrollContentBackground(.hidden)
                .disabled(viewModel.busy)
                .frame(minHeight: 120)
        }
        .background(Color(.secondarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color(.separator), lineWidth: 1)
        )
    }
}
