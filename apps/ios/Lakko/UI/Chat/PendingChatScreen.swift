// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The chat with a person added through their invite link whose app has not
/// answered yet (ADR-0079 section 5, `docs/ux/contact-invite-link.md` section
/// 4). Mirror of `apps/web/src/routes/PendingChat.tsx`.
///
/// One line at the top of the timeline says the person gets the messages when
/// their app is next online (after 7 days: that they have not answered yet).
/// What is written is kept on this device, shown with the clock mark of any
/// unsent message, and sent in order once the inviter's Welcome lands; the
/// screen then hands over to the real chat. The call buttons are disabled.
struct PendingChatScreen: View {
    let accountIdHex: String
    @ObservedObject var contacts: ContactsRepository
    let chat: ChatRepository
    /// The one-to-one exists now: replace this screen with it.
    let onConnected: (String) -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var draft = ""
    @State private var messages: [HeldSendQueue.Entry] = []
    @State private var callHintShown = false

    private var contact: PendingContact? { contacts.pendingContact(accountIdHex: accountIdHex) }

    private var name: String {
        contact.flatMap(PendingContacts.name) ?? l10n.t("invite.contact.page.unnamed")
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .trailing, spacing: 8) {
                    if let contact {
                        Text(lineText(contact))
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                    }
                    ForEach(messages, id: \.localId) { entry in
                        HStack(alignment: .bottom, spacing: 6) {
                            SwiftUI.Group {
                                if entry.text.isEmpty {
                                    Image(systemName: "paperclip")
                                } else {
                                    Text(entry.text)
                                }
                            }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                                .background(Color.lakkoAccentSoft)
                                .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                            Image(systemName: "clock")
                                .font(.caption2)
                                .foregroundStyle(Color.lakkoTextMuted)
                                .accessibilityLabel(l10n.t("chat.pending.waiting", texts: ["name": name]))
                        }
                    }
                }
                .padding(16)
            }
            if callHintShown {
                Text(l10n.t("chat.pending.call_disabled", texts: ["name": name]))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 4)
            }
            composer
        }
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            // ADR-0084 — the person's photo beside their name, as in a chat.
            ToolbarItem(placement: .principal) {
                HStack(spacing: 8) {
                    InitialsAvatar(
                        name: name, identity: accountIdHex, size: 32,
                        photo: contact.map { AvatarPhoto.person($0.signaturePubkey) },
                        // UX profile-screen section 7: the chat header plays
                        // the video always on a touch platform.
                        playsVideo: true)
                    Text(name).font(.headline).lineLimit(1)
                }
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                // Disabled until the inviter's app answers; a tap on the
                // disabled control shows the hint (a disabled Button takes no
                // tap, so the hint rides a gesture on its label).
                Image(systemName: "phone")
                    .foregroundStyle(Color.lakkoTextMuted)
                    .onTapGesture { callHintShown = true }
                    .accessibilityLabel(l10n.t("chat.pending.call_disabled", texts: ["name": name]))
                Image(systemName: "video")
                    .foregroundStyle(Color.lakkoTextMuted)
                    .onTapGesture { callHintShown = true }
                    .accessibilityLabel(l10n.t("chat.pending.call_disabled", texts: ["name": name]))
            }
        }
        .onAppear {
            reload()
            followIfConnected()
        }
        .onChange(of: contacts.handedOver) { _, _ in followIfConnected() }
    }

    private var composer: some View {
        HStack(spacing: 8) {
            TextField(l10n.t("chat.composer.placeholder"), text: $draft, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1...5)
            Button {
                send()
            } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title2)
            }
            .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || contact == nil)
        }
        .padding(12)
    }

    private func lineText(_ contact: PendingContact) -> String {
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        switch PendingContacts.line(contact, nowMs: nowMs) {
        case .waiting: return l10n.t("chat.pending.waiting", texts: ["name": name])
        case .stale: return l10n.t("chat.pending.stale", texts: ["name": name])
        }
    }

    private func reload() {
        guard let contact else { return }
        // Pairing state bundle v3 — what another device of this account wrote
        // to the person is shown here too, as display-only copies in the same
        // waiting state; the device it was typed on sends it.
        let own = chat.pendingInvitedMessages(queueKey: contact.queueKey)
        let copies = chat.pendingInvitedDisplayCopies(queueKey: contact.queueKey)
        messages = Self.merged(own: own, copies: copies)
    }

    /// This device's own messages and the display copies, oldest first (a
    /// stable order: on a tie, own messages first, each in its queue order).
    static func merged(own: [HeldSendQueue.Entry], copies: [HeldSendQueue.Entry]) -> [HeldSendQueue.Entry] {
        let all = own.enumerated().map { (0, $0.offset, $0.element) }
            + copies.enumerated().map { (1, $0.offset, $0.element) }
        return all.sorted { a, b in
            if a.2.queuedAtMs != b.2.queuedAtMs { return a.2.queuedAtMs < b.2.queuedAtMs }
            return (a.0, a.1) < (b.0, b.1)
        }.map(\.2)
    }

    private func send() {
        guard let contact else { return }
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if chat.queuePendingInvitedMessage(queueKey: contact.queueKey, text: text) {
            draft = ""
            reload()
        }
    }

    private func followIfConnected() {
        if let groupIdHex = contacts.handedOver[accountIdHex] {
            onConnected(groupIdHex)
        }
    }
}
