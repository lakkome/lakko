// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `PasskeysView` — manage the user's stored passkeys.
///
/// Mirror of `apps/web/src/auth/PasskeysSection.tsx`. Surfaces, in this
/// order:
///
///   1. The registered passkeys: nickname, timestamps, a label that is a
///      state and not a setting ("Logs in and restores keys" / "Logs in
///      only"), on a passkey that holds no keys the repair "Restore keys
///      with this passkey", and swipe to remove.
///   2. The recovery phrase input of that repair, when this device does not
///      hold the phrase.
///   3. Nickname input + "Add passkey" button driving
///      ``WebauthnRegistration`` on tap; the keys are then stored for the
///      new passkey with nothing to choose (UX 10.2).
///   4. A single-line status banner that flips between success
///      and error.
///
/// Reachable from Settings > Passkeys; pushed onto the Settings stack, so
/// the system back button returns there.
public struct PasskeysView: View {

    @StateObject private var viewModel: PasskeysViewModel
    /// A3 — drives the delete-account confirmation dialog.
    @State private var confirmingDelete = false
    /// The data export the system's save picker is up for.
    @State private var exportingData: ExportedFile?
    @ObservedObject private var l10n = Localization.shared

    public init(viewModel: @autoclosure @escaping () -> PasskeysViewModel) {
        self._viewModel = StateObject(wrappedValue: viewModel())
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(l10n.t("settings.passkeys.lede"))
                .font(.footnote)
                .foregroundStyle(.secondary)

                if viewModel.isLoading {
                    ProgressView(l10n.t("common.loading"))
                        .padding(.vertical, 8)
                } else if viewModel.credentials.isEmpty {
                    Text(l10n.t("settings.passkeys.empty"))
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 4)
                    if viewModel.keysResetThisSession {
                        // UX 15.3 — the reset deleted every passkey.
                        Text(l10n.t("settings.passkeys.reset_note"))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .padding(.vertical, 4)
                    }
                } else {
                    List {
                        ForEach(viewModel.credentials) { cred in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(cred.nickname.isEmpty
                                    ? l10n.t("settings.passkeys.unnamed")
                                    : cred.nickname)
                                    .font(.body)
                                Text(metadataLine(for: cred))
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                                keysLine(for: cred)
                            }
                            .swipeActions {
                                Button(role: .destructive) {
                                    Task { await viewModel.deleteCredential(cred) }
                                } label: {
                                    Label(l10n.t("common.remove"), systemImage: "trash")
                                }
                            }
                        }
                    }
                    .listStyle(.plain)
                    .scrollDisabled(true)
                    // Rebuilt on a layout-direction switch (a UIKit-backed
                    // List keeps the mirroring it was built with).
                    .id(l10n.layoutDirection)
                    .frame(minHeight: 100, maxHeight: 360)
                }

                if let credentialIdB64 = viewModel.repairAskingPhrase {
                    repairPhraseCard
                        .id(credentialIdB64)
                }

                Divider()
                    .padding(.vertical, 4)

                Text(l10n.t("settings.passkeys.name.label"))
                    .font(.headline)

                TextField(
                    l10n.t("settings.passkeys.name.placeholder"),
                    text: $viewModel.nickname
                )
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)
                .textFieldStyle(.roundedBorder)
                .disabled(viewModel.isBusy)

                // UX 17.1 — adding a passkey needs a fresh-auth proof: the
                // block is here while none is live.
                FreshAuthBlock(model: viewModel.addProof)

                Button {
                    Task {
                        let anchor = currentPresentationAnchor()
                        await viewModel.addPasskey(presentationAnchor: anchor)
                    }
                } label: {
                    Text(l10n.t(viewModel.isBusy ? "common.adding" : "settings.passkeys.add"))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(viewModel.isBusy || !viewModel.addProof.layout.ready)

                if let message = viewModel.message {
                    statusBanner(for: message)
                        .padding(.top, 4)
                }

                Divider().padding(.vertical, 12)

                // A3 (GDPR) — data portability.
                Text(l10n.t("settings.data.title"))
                Text(l10n.t("settings.data.lede"))
                .font(.footnote)
                .foregroundStyle(.secondary)
                Button {
                    Task { await viewModel.exportData() }
                } label: {
                    Text(l10n.t("settings.data.export"))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .disabled(viewModel.isBusy)
                if let url = viewModel.exportFileUrl {
                    // The export is a file leaving the app: it goes through
                    // the system's save picker, never the share sheet.
                    Button {
                        exportingData = ExportedFile(url: url)
                    } label: {
                        Label(l10n.t("settings.data.share"), systemImage: "square.and.arrow.down")
                    }
                }

                Divider().padding(.vertical, 12)

                // A3 (GDPR) — permanent account deletion (danger zone).
                Button(role: .destructive) {
                    confirmingDelete = true
                } label: {
                    Text(l10n.t("settings.account.delete.confirm"))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .disabled(viewModel.isBusy)
                .fileExport($exportingData)
                Text(l10n.t("settings.account.delete.sub"))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.top, 4)

            }
            .padding(16)
        }
        .navigationTitle(l10n.t("settings.passkeys.title"))
        .navigationBarTitleDisplayMode(.inline)
        // UX 17.3 — the app's own dialog: it carries the proof block.
        .sheet(isPresented: $confirmingDelete) {
            DeleteAccountDialog(viewModel: viewModel, onClose: { confirmingDelete = false })
                .presentationDetents([.medium, .large])
        }
        .task {
            await viewModel.refresh()
            await viewModel.addProof.opened()
        }
        .onDisappear { viewModel.addProof.closed() }
    }

    /// The label saying what the passkey does (a state, not a setting) and,
    /// on a passkey that holds no keys but may carry them, the repair.
    @ViewBuilder
    private func keysLine(for cred: CredentialSummary) -> some View {
        let label = viewModel.keysLabel(for: cred)
        HStack(spacing: 8) {
            Text(l10n.t(label.catalogKey))
                .font(.footnote)
                .foregroundStyle(.secondary)
            Spacer()
            if viewModel.offersRestoreKeys(for: cred) {
                Button(l10n.t("settings.passkeys.keys.turn_on")) {
                    Task { await viewModel.restoreKeys(for: cred) }
                }
                .font(.footnote)
                .buttonStyle(.borderless)
                .disabled(viewModel.isBusy)
            }
        }
    }

    /// The twelve-slot phrase input of "Restore keys with this passkey",
    /// shown when this device does not hold the recovery phrase.
    private var repairPhraseCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(l10n.t("settings.passkeys.keys_choice.phrase_needed"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
            PhraseEntryGrid(phrase: $viewModel.repairPhrase, disabled: viewModel.isBusy)
            HStack(spacing: 12) {
                AuthSecondaryButton(
                    title: l10n.t(viewModel.isBusy ? "common.checking" : "settings.passkeys.keys.turn_on"),
                    disabled: viewModel.isBusy,
                    action: { Task { await viewModel.submitRepairPhrase() } }
                )
                AuthSecondaryButton(
                    title: l10n.t("common.cancel"),
                    disabled: viewModel.isBusy,
                    action: viewModel.cancelRepair
                )
            }
        }
        .padding(16)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private func statusBanner(for message: PasskeysViewModel.StatusMessage) -> some View {
        switch message {
        case .ok(let text):
            Label(text.text, systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.footnote)
        case .error(let text):
            Label(text.text, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .font(.footnote)
        }
    }

    /// Dates in the interface language (ADR-0061: formatted by the platform
    /// before they are passed).
    private func metadataLine(for cred: CredentialSummary) -> String {
        let created = format(cred.created_at_ms) ?? l10n.t("common.unknown")
        if let lastUsedMs = cred.last_used_at_ms, let lastUsed = format(lastUsedMs) {
            return l10n.t("settings.passkeys.added_last_used", texts: ["added": created, "used": lastUsed])
        }
        return l10n.t("settings.passkeys.added_never_used", texts: ["added": created])
    }

    private func format(_ ms: Int64?) -> String? {
        guard let ms else { return nil }
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000.0)
        let formatter = DateFormatter()
        formatter.locale = l10n.locale
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}
