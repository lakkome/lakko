// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// A contact's detail (UX spec section 5.3): 72 pt avatar, name, `@username`;
/// Message, Call and Video; "Verify safety number" and Rename. No key
/// fingerprint anywhere on it — the safety number is the verification surface.
///
/// Pushed from the Contacts list through ``TabRoute/contactDetail(messageId:)``,
/// and presented as a sheet from a conversation's header
/// (`docs/ux/conversation-info.md` section 1): one person screen, two ways of
/// showing it, because a second one would be the same drift the conversation
/// header's two routes to one panel already produced.
/// "Message" opens the 1x1 chat through the same path the list used to; Call
/// and Video are the chat header's entry points and are disabled, with the
/// reason shown, when no chat exists yet.
///
/// "Remove contact" is not offered: no client has a remove-contact operation
/// (the accepted list only ever grows), so there is nothing existing to call.
struct ContactDetailScreen: View {
    /// How this screen is being shown (`docs/ux/conversation-info.md`
    /// section 3). Pushed, it sits in the stack that pushed it and that stack
    /// resolves ``TabRoute/safetyNumber(messageId:)``; as a sheet it is a
    /// layer over the conversation and carries its own stack and its own way
    /// out, because neither is there to inherit.
    enum Presentation {
        case pushed
        case sheet
    }

    @ObservedObject var viewModel: ContactsViewModel
    let messageId: Int64
    /// Open the 1x1 chat.
    let onOpenChat: (String) -> Void
    /// Start a call in the 1x1 chat; `video` asks for the camera.
    let onCall: (_ groupIdHex: String, _ video: Bool) -> Void
    var presentation: Presentation = .pushed
    /// Section 2.1 — "Message, when the info was opened from somewhere other
    /// than the chat". Opened from the chat itself it would lead back to the
    /// screen behind the sheet.
    var offersMessage: Bool = true
    /// Close the layer. Only reached in ``Presentation/sheet``.
    var onDone: () -> Void = {}

    @State private var renaming = false
    @State private var draftName = ""

    var body: some View {
        switch presentation {
        case .pushed:
            content
        case .sheet:
            NavigationStack {
                content
                    // The safety number is a `NavigationLink(value:)`, so in a
                    // sheet it needs a stack that answers for it. Pushed, the
                    // tab's stack does.
                    .navigationDestination(for: TabRoute.self) { route in
                        if case .safetyNumber(let id) = route {
                            SafetyNumberScreen(viewModel: viewModel, messageId: id)
                        }
                    }
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button("Done", action: onDone)
                        }
                    }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        if let contact = viewModel.contact(messageId: messageId) {
            ResolvedName(signatureKey: contact.request.fromPubkey) { resolved in
                detail(contact: contact, resolved: resolved)
            }
        } else {
            ContentUnavailableView(
                "Contact not found",
                systemImage: "person.crop.circle.badge.questionmark",
                description: Text("This person is no longer in your contacts.")
            )
            .navigationTitle("Contact")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    @ViewBuilder
    private func detail(contact: AcceptedContact, resolved: ResolvedDisplayName) -> some View {
        let chat = viewModel.chatGroupIdHex(for: contact)
        let callable = chat.map { viewModel.canCall(groupIdHex: $0) } ?? false
        List {
            Section {
                VStack(spacing: 8) {
                    InitialsAvatar(
                        name: resolved.isPlaceholder ? "" : resolved.name,
                        identity: base64Encode(contact.request.fromPubkey),
                        size: 72,
                        placeholderSymbol: "person.fill"
                    )
                    Text(resolved.name)
                        .font(.title2.weight(.semibold))
                        .foregroundStyle(Color.lakkoText)
                        .multilineTextAlignment(.center)
                    if let line = resolved.usernameLine {
                        Text(line)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if let theirName = resolved.theirName {
                        Text("Their name: \(theirName)")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if resolved.usernameNotVerified {
                        Text("Username not verified")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if chat == nil {
                        Text("Chat not set up yet")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
                .listRowBackground(Color.clear)
            }

            Section {
                HStack(spacing: 12) {
                    if offersMessage {
                        ContactActionButton(
                            title: "Message", systemImage: "bubble.left.fill", enabled: chat != nil
                        ) {
                            if let chat { onOpenChat(chat) }
                        }
                    }
                    ContactActionButton(title: "Call", systemImage: "phone.fill", enabled: callable) {
                        if let chat { onCall(chat, false) }
                    }
                    ContactActionButton(title: "Video", systemImage: "video.fill", enabled: callable) {
                        if let chat { onCall(chat, true) }
                    }
                }
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
            }

            Section {
                NavigationLink(value: TabRoute.safetyNumber(messageId: messageId)) {
                    Label("Verify safety number", systemImage: "checkmark.shield")
                        .foregroundStyle(Color.lakkoText)
                }
                Button {
                    draftName = contact.displayName ?? ""
                    renaming = true
                } label: {
                    Label("Rename", systemImage: "pencil")
                        .foregroundStyle(Color.lakkoText)
                }
            } footer: {
                Text("Compare safety numbers in person or on a call to be sure nobody is between you.")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(resolved.name)
        .navigationBarTitleDisplayMode(.inline)
        .alert("Rename contact", isPresented: $renaming) {
            TextField("Display name", text: $draftName)
            Button("Save") {
                viewModel.onRenameContactClicked(messageId: contact.request.messageId, name: draftName)
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Only you see this name, in chats and call rosters. "
                    + "Leave blank to use the name they set."
            )
        }
    }
}

/// One of the three actions under the header: icon over a label, on a card.
private struct ContactActionButton: View {
    let title: String
    let systemImage: String
    let enabled: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            VStack(spacing: 6) {
                Image(systemName: systemImage)
                    .font(.title3)
                Text(title)
                    .font(.caption.weight(.medium))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            .foregroundStyle(enabled ? Color.lakkoAccent : Color.lakkoTextMuted)
            .background(Color.lakkoSurface)
            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityLabel(title)
    }
}

// MARK: - Safety number

/// Contact > Verify safety number: the 60 digits, five to a group, three
/// groups to a row, large enough to read to each other, and one line on what
/// a match means. Computed by the existing
/// ``ContactsRepository/safetyNumber(against:)``.
///
/// There is no "Mark as verified" toggle: no local store records a verified
/// contact, and a toggle that persisted nothing would be a lie.
struct SafetyNumberScreen: View {
    @ObservedObject var viewModel: ContactsViewModel
    let messageId: Int64

    @State private var copied = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let contact = viewModel.contact(messageId: messageId) {
                    if let number = viewModel.safetyNumber(for: contact) {
                        ResolvedName(signatureKey: contact.request.fromPubkey) { resolved in
                            Text(
                                "Compare this number with the one "
                                    + (resolved.isPlaceholder ? "this person" : resolved.name)
                                    + " sees on their device."
                            )
                            .font(.body)
                            .foregroundStyle(Color.lakkoTextMuted)
                        }
                        SafetyNumberGrid(number: number)
                        Text("If both of you see the same number, nobody is between you and your messages and calls stay private.")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                        Button {
                            UIPasteboard.general.string = number
                            copied = true
                            Task { @MainActor in
                                try? await Task.sleep(nanoseconds: 1_500_000_000)
                                copied = false
                            }
                        } label: {
                            Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                        }
                        .buttonStyle(.bordered)
                    } else {
                        Text("Sign in again to show the safety number.")
                            .foregroundStyle(Color.lakkoDanger)
                    }
                } else {
                    Text("This person is no longer in your contacts.")
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .padding(16)
        }
        .background(Color.lakkoBackground)
        .navigationTitle("Safety number")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// The digits laid out by ``SafetyNumberFormat``.
private struct SafetyNumberGrid: View {
    let number: String

    var body: some View {
        let rows = SafetyNumberFormat.rows(number)
        VStack(spacing: 12) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                HStack(spacing: 0) {
                    ForEach(Array(row.enumerated()), id: \.offset) { _, group in
                        Text(group)
                            .font(.system(.title2, design: .monospaced).weight(.medium))
                            .foregroundStyle(Color.lakkoText)
                            .frame(maxWidth: .infinity)
                    }
                }
            }
        }
        .padding(.vertical, 20)
        .padding(.horizontal, 12)
        .frame(maxWidth: .infinity)
        .background(Color.lakkoSurfaceSunken)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Safety number")
        .accessibilityValue(SafetyNumberFormat.groups(number).joined(separator: ", "))
    }
}
