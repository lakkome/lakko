// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Contacts > Add by username (UX spec section 5.3): the username field, an
/// optional intro, Send. A successful send dismisses the sheet; the request
/// then shows under the list's "Sent" header until the other person accepts.
///
/// The fields live on ``ContactsViewModel`` (the same `onSendClicked` path
/// the previous inline form drove), so a draft survives the sheet being
/// swiped down and reopened.
struct AddContactSheet: View {
    @ObservedObject var viewModel: ContactsViewModel
    let onDismiss: () -> Void
    /// ADR-0058 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    @FocusState private var usernameFocused: Bool

    private var canSend: Bool {
        guard !viewModel.sending, viewModel.usernameHint == nil else { return false }
        return !viewModel.usernameInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(l10n.t("contacts.add.username_placeholder"), text: $viewModel.usernameInput)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.asciiCapable)
                        .submitLabel(.send)
                        .focused($usernameFocused)
                        .onSubmit { if canSend { viewModel.onSendClicked() } }
                        .disabled(viewModel.sending)
                    UsernameRuleHint(input: viewModel.usernameInput)
                } header: {
                    Text(l10n.t("contacts.add.username_label"))
                } footer: {
                    Text(l10n.t("contacts.add.subtitle"))
                }

                Section {
                    TextField(l10n.t("contacts.add.intro_placeholder"), text: $viewModel.introInput, axis: .vertical)
                        .lineLimit(2...4)
                        .disabled(viewModel.sending)
                } header: {
                    Text(l10n.t("contacts.add.intro_label"))
                }

                if let error = viewModel.sendError {
                    Section {
                        Text(error.text)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            .navigationTitle(l10n.t("contacts.add.title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(l10n.t("common.cancel"), action: onDismiss)
                        .disabled(viewModel.sending)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    if viewModel.sending {
                        ProgressView()
                    } else {
                        Button(l10n.t("contacts.add.send")) { viewModel.onSendClicked() }
                            .disabled(!canSend)
                    }
                }
            }
            .onAppear { usernameFocused = true }
        }
        .onChange(of: viewModel.sendSucceededCount) { _, _ in onDismiss() }
        .interactiveDismissDisabled(viewModel.sending)
        .presentationDetents([.medium, .large])
    }
}
