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
/// Reachable from ``HomeScreen`` via the "Manage passkeys"
/// navigation row. Wraps itself in a `NavigationStack` for
/// titled-screen affordances.
public struct PasskeysView: View {

    @StateObject private var viewModel: PasskeysViewModel
    public var onBack: () -> Void
    /// A3 — drives the delete-account confirmation dialog.
    @State private var confirmingDelete = false

    public init(
        viewModel: @autoclosure @escaping () -> PasskeysViewModel,
        onBack: @escaping () -> Void
    ) {
        self._viewModel = StateObject(wrappedValue: viewModel())
        self.onBack = onBack
    }

    public var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                Text(
                    "Sign in with Face ID, Touch ID, or a hardware key. The "
                        + "24-word recovery phrase always works as the last-resort path."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)

                if viewModel.isLoading {
                    ProgressView("Loading passkeys…")
                        .padding(.vertical, 8)
                } else if viewModel.credentials.isEmpty {
                    Text("No passkeys yet. Add one below.")
                        .foregroundStyle(.secondary)
                        .padding(.vertical, 4)
                } else {
                    List {
                        ForEach(viewModel.credentials) { cred in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(cred.nickname.isEmpty
                                    ? "(unnamed passkey)"
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
                                    Label("Delete", systemImage: "trash")
                                }
                            }
                        }
                    }
                    .listStyle(.plain)
                    .frame(minHeight: 100, maxHeight: 280)
                }

                Divider()
                    .padding(.vertical, 4)

                Text("Add a passkey")
                    .font(.headline)

                TextField(
                    "Nickname (e.g. iPhone Face ID)",
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
                    Text(viewModel.isBusy ? "Adding…" : "Add passkey")
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

                // A3 (GDPR) — permanent account deletion (danger zone).
                Button(role: .destructive) {
                    confirmingDelete = true
                } label: {
                    Text("Delete my account")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.large)
                .disabled(viewModel.isBusy)
                Text(
                    "Permanently erase your account from the server (username, "
                        + "keys, recovery factors, backup) and wipe this device. "
                        + "This cannot be undone."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.top, 4)
                .confirmationDialog(
                    "Delete your account permanently?",
                    isPresented: $confirmingDelete,
                    titleVisibility: .visible
                ) {
                    Button("Delete my account", role: .destructive) {
                        Task { await viewModel.deleteAccount() }
                    }
                    Button("Cancel", role: .cancel) {}
                }

                Spacer()
            }
            .padding(20)
            .navigationTitle("Passkeys")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button("Back", action: onBack)
                }
            }
        }
        .task {
            await viewModel.refresh()
        }
    }

    @ViewBuilder
    private func statusBanner(for message: PasskeysViewModel.StatusMessage) -> some View {
        switch message {
        case .ok(let text):
            Label(text, systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .font(.footnote)
        case .error(let text):
            Label(text, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .font(.footnote)
        }
    }

    private func metadataLine(for cred: CredentialSummary) -> String {
        let created = Self.format(cred.created_at_ms) ?? "unknown"
        if let lastUsedMs = cred.last_used_at_ms, let lastUsed = Self.format(lastUsedMs) {
            return "Added \(created) · last used \(lastUsed)"
        }
        return "Added \(created) · never used"
    }

    private static func format(_ ms: Int64?) -> String? {
        guard let ms else { return nil }
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000.0)
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}
