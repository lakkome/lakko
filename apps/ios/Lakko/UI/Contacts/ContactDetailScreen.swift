// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// A contact's detail (UX spec section 5.3): 72 pt avatar, name, `@username`;
/// Message, Call and Video; "Verify safety number", Rename and Remove. No key
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
/// **Which rows this holds is `personInfoSections`, not a flag read here.**
/// The rule is shared with the web and Android clients
/// (`crates/lakko-ui-rules`); a panel reading its own flags is exactly the
/// defect that replaced. Two of its answers were never built on iOS until
/// now: "Remove contact" — the file used to claim no client had one, while
/// Android's `removeContact` and web's `removeAccepted` had both shipped —
/// and "Delete this chat" on a conversation this device's membership has
/// ended, which lived only in the group panel a one-to-one had stopped
/// opening, leaving an ended one-to-one with no way out at all.
struct ContactDetailScreen: View {
    /// How this screen is being shown (`docs/ux/conversation-info.md`
    /// section 3). Pushed, it sits in the stack that pushed it and that stack
    /// resolves ``TabRoute/safetyNumber(messageId:)``; as a sheet it is a
    /// layer over the conversation and carries its own stack and its own way
    /// out, because neither is there to inherit.
    @ObservedObject var viewModel: ContactsViewModel
    let messageId: Int64
    /// Open the 1x1 chat.
    let onOpenChat: (String) -> Void
    /// Start a call in the 1x1 chat; `video` asks for the camera.
    let onCall: (_ groupIdHex: String, _ video: Bool) -> Void
    /// The chat is gone and whatever is behind this has nothing left to show
    /// (ADR-0045 UX 6.2.4). Called only after a delete actually landed.
    var onChatDeleted: () -> Void = {}

    @State private var renaming = false
    @State private var draftName = ""
    @State private var confirmingRemove = false
    @State private var confirmingDeleteChat = false
    /// ADR-0058 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        // One presentation: pushed on the tab's stack, from the Contacts list
        // and from a chat's header alike (`docs/ux/conversation-info.md`
        // section 3, "Except a person"). The sheet presentation, and the
        // origin that hid Message on it, gave one contact two cards.
        content
    }

    @ViewBuilder
    private var content: some View {
        if let contact = viewModel.contact(messageId: messageId) {
            ResolvedName(signatureKey: contact.request.fromPubkey) { resolved in
                detail(contact: contact, resolved: resolved)
            }
        } else {
            ContentUnavailableView(
                l10n.t("contacts.detail.gone_title"),
                systemImage: "person.crop.circle.badge.questionmark",
                description: Text(l10n.t("contacts.detail.gone_body"))
            )
            .navigationTitle(l10n.t("chat.info.contact"))
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    @ViewBuilder
    private func detail(contact: AcceptedContact, resolved: ResolvedDisplayName) -> some View {
        let chat = viewModel.chatGroupIdHex(for: contact)
        let callable = chat.map { viewModel.canCall(groupIdHex: $0) } ?? false
        // Section 2.1, decided once in `crates/lakko-ui-rules` where the host
        // test suite can run it, never by a flag read down in the layout. The
        // contact is in the list by construction — this branch only draws when
        // `viewModel.contact(messageId:)` found one.
        let sections = personInfoSections(
            caps: PersonInfoCapabilities(
                membershipEnded: chat.map { viewModel.membershipEnded(groupIdHex: $0) } ?? false))
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
                        Text(l10n.t("contacts.detail.their_name", texts: ["name": theirName]))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if resolved.usernameNotVerified {
                        Text(l10n.t("contacts.detail.username_not_verified"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    if chat == nil {
                        Text(l10n.t("chats.new_chat.not_available"))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 8)
                .listRowBackground(Color.clear)
            }

            // The tiles go entirely when the rule offers neither, rather than
            // leaving an empty row where the actions used to be. An ended
            // membership takes `Call` away for the reason ADR-0045 UX 6.2.8
            // gives: the per-call key comes from a group this device is no
            // longer in, so the control would assert a live session there is
            // not.
            if sections.contains(.call) || sections.contains(.message) {
                Section {
                    HStack(spacing: 12) {
                        if sections.contains(.message) {
                            ContactActionButton(
                                title: l10n.t("contacts.detail.message"), systemImage: "bubble.left.fill",
                                enabled: chat != nil
                            ) {
                                if let chat { onOpenChat(chat) }
                            }
                        }
                        if sections.contains(.call) {
                            ContactActionButton(
                                title: l10n.t("chat.call.voice"), systemImage: "phone.fill", enabled: callable
                            ) {
                                if let chat { onCall(chat, false) }
                            }
                            ContactActionButton(
                                title: l10n.t("chat.media.video"), systemImage: "video.fill", enabled: callable
                            ) {
                                if let chat { onCall(chat, true) }
                            }
                        }
                    }
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
                }
            }

            Section {
                if sections.contains(.safetyNumber) {
                    NavigationLink(value: TabRoute.safetyNumber(messageId: messageId)) {
                        Label(l10n.t("contacts.safety.title"), systemImage: "checkmark.shield")
                            .foregroundStyle(Color.lakkoText)
                    }
                }
                if sections.contains(.renameContact) {
                    Button {
                        draftName = contact.displayName ?? ""
                        renaming = true
                    } label: {
                        Label(l10n.t("contacts.rename.action"), systemImage: "pencil")
                            .foregroundStyle(Color.lakkoText)
                    }
                }
            } footer: {
                if sections.contains(.safetyNumber) {
                    Text(l10n.t("contacts.safety.row_sub"))
                }
            }

            // The two destructive controls, in their own block at the bottom
            // where iOS puts what cannot be undone. They are different acts on
            // different things — one forgets the PERSON, the other forgets the
            // CONVERSATION — and the rule can return both at once, so neither
            // is worded as if it did the other's job.
            if sections.contains(.removeContact) || sections.contains(.deleteChat) {
                Section {
                    if sections.contains(.removeContact) {
                        Button(role: .destructive) { confirmingRemove = true } label: {
                            Label(l10n.t("contacts.remove.control"), systemImage: "person.badge.minus")
                                // A destructive row is red end to end; the
                                // symbol used to keep the platform's blue tint
                                // (seen live 2026-09-26).
                                .foregroundStyle(Color.lakkoDanger)
                        }
                    }
                    if sections.contains(.deleteChat), chat != nil {
                        Button(role: .destructive) { confirmingDeleteChat = true } label: {
                            Text(deleteChatControl)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(resolved.name)
        .navigationBarTitleDisplayMode(.inline)
        .alert(l10n.t("contacts.rename.action"), isPresented: $renaming) {
            TextField(l10n.t("contacts.rename.placeholder"), text: $draftName)
            Button(l10n.t("common.save")) {
                viewModel.onRenameContactClicked(messageId: contact.request.messageId, name: draftName)
            }
            Button(l10n.t("common.cancel"), role: .cancel) {}
        } message: {
            Text(l10n.t("contacts.rename.sub_local"))
        }
        // Local only, and the body says exactly that: the peer is not told,
        // and an existing chat is deliberately left behind, because leaving a
        // conversation is the conversation's own action.
        .confirmationDialog(
            l10n.t("contacts.remove.title"),
            isPresented: $confirmingRemove,
            titleVisibility: .visible
        ) {
            Button(
                l10n.t("contacts.remove.confirm_named", texts: ["name": resolved.name]),
                role: .destructive
            ) {
                viewModel.onRemoveContactClicked(messageId: contact.request.messageId)
            }
            Button(l10n.t("common.cancel"), role: .cancel) {}
        } message: {
            Text(l10n.t("contacts.remove.body"))
        }
        // ADR-0045 UX 6.2.4 — the same control, the same words and the same
        // repository call as the group panel's. A second set of words for one
        // act is how the three clients drifted in the first place.
        .confirmationDialog(
            deleteChatTitle,
            isPresented: $confirmingDeleteChat,
            titleVisibility: .visible
        ) {
            Button(l10n.t("common.delete"), role: .destructive) {
                // Route away only when the delete actually landed; a refusal
                // leaves the screen where it is rather than dropping the user
                // out of a chat that is still there.
                if let chat, viewModel.onDeleteChatClicked(groupIdHex: chat) {
                    onChatDeleted()
                }
            }
            Button(l10n.t("common.cancel"), role: .cancel) {}
        } message: {
            Text(deleteChatBody)
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
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let contact = viewModel.contact(messageId: messageId) {
                    if let number = viewModel.safetyNumber(for: contact) {
                        ResolvedName(signatureKey: contact.request.fromPubkey) { resolved in
                            Text(resolved.isPlaceholder
                                ? l10n.t("contacts.safety.body_unnamed")
                                : l10n.t("contacts.safety.body", texts: ["name": resolved.name]))
                            .font(.body)
                            .foregroundStyle(Color.lakkoTextMuted)
                        }
                        SafetyNumberGrid(number: number)
                        Button {
                            UIPasteboard.general.string = number
                            copied = true
                            Task { @MainActor in
                                try? await Task.sleep(nanoseconds: 1_500_000_000)
                                copied = false
                            }
                        } label: {
                            Label(l10n.t(copied ? "common.copied" : "common.copy"), systemImage: copied ? "checkmark" : "doc.on.doc")
                        }
                        .buttonStyle(.bordered)
                    } else {
                        Text(l10n.t("contacts.safety.sign_in"))
                            .foregroundStyle(Color.lakkoDanger)
                    }
                } else {
                    Text(l10n.t("contacts.detail.gone_body"))
                        .foregroundStyle(Color.lakkoTextMuted)
                }
            }
            .padding(16)
        }
        .background(Color.lakkoBackground)
        .navigationTitle(l10n.t("contacts.safety.number"))
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
        .accessibilityLabel(Localization.text("contacts.safety.number"))
        .accessibilityValue(SafetyNumberFormat.groups(number).joined(separator: ", "))
    }
}
