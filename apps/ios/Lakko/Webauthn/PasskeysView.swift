// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `PasskeysView` — manage the user's stored passkeys.
///
/// Mirror of `apps/web/src/auth/PasskeysSection.tsx`. Surfaces
/// three things in this order:
///
///   1. List of registered passkeys (nickname + timestamps +
///      per-row Delete).
///   2. Nickname input + "Add passkey" button driving
///      ``WebauthnRegistration`` on tap.
///   3. A single-line status banner that flips between success
///      and error toasts.
///
/// Reachable from Settings > Passkeys; pushed onto the Settings stack, so
/// the system back button returns there.
public struct PasskeysView: View {

    @StateObject private var viewModel: PasskeysViewModel
    /// A3 — drives the delete-account confirmation dialog.
    @State private var confirmingDelete = false
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
                    .frame(minHeight: 100, maxHeight: 280)
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
                .disabled(viewModel.isBusy)

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
                    ShareLink(item: url) {
                        Label(l10n.t("settings.data.share"), systemImage: "square.and.arrow.up")
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
                Text(l10n.t("settings.account.delete.sub"))
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.top, 4)
                .confirmationDialog(
                    l10n.t("settings.account.delete.title"),
                    isPresented: $confirmingDelete,
                    titleVisibility: .visible
                ) {
                    Button(l10n.t("settings.account.delete.confirm"), role: .destructive) {
                        Task { await viewModel.deleteAccount() }
                    }
                    Button(l10n.t("common.cancel"), role: .cancel) {}
                } message: {
                    Text(l10n.t("settings.account.delete.body"))
                }

            }
            .padding(16)
        }
        .navigationTitle(l10n.t("settings.passkeys.title"))
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await viewModel.refresh()
        }
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

    /// Dates in the interface language (ADR-0058: formatted by the platform
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
