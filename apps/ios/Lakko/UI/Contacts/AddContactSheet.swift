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

    @FocusState private var usernameFocused: Bool

    private var canSend: Bool {
        guard !viewModel.sending, viewModel.usernameHint == nil else { return false }
        return !viewModel.usernameInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Username", text: $viewModel.usernameInput)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.asciiCapable)
                        .submitLabel(.send)
                        .focused($usernameFocused)
                        .onSubmit { if canSend { viewModel.onSendClicked() } }
                        .disabled(viewModel.sending)
                    UsernameRuleHint(input: viewModel.usernameInput)
                } header: {
                    Text("Username")
                } footer: {
                    Text("They get a request they can accept or decline.")
                }

                Section {
                    TextField("Say who you are (optional)", text: $viewModel.introInput, axis: .vertical)
                        .lineLimit(2...4)
                        .disabled(viewModel.sending)
                } header: {
                    Text("Message")
                }

                if let error = viewModel.sendError {
                    Section {
                        Text(error)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            .navigationTitle("Add contact")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel", action: onDismiss)
                        .disabled(viewModel.sending)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    if viewModel.sending {
                        ProgressView()
                    } else {
                        Button("Send") { viewModel.onSendClicked() }
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
