// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `Participants` on a call that can hand out a link to itself
/// (`docs/ux/call-links.md` section 3): a menu of the app's own, above the
/// control bar.
///
/// ## Why this is not the share sheet, and not a system menu
///
/// The first version handed the link to the iOS share sheet: a foreign
/// surface in the middle of a call, offering a dozen destinations where two
/// are wanted (found live 2026-10-02). The rule is general and lives in the
/// design system: a Lakko screen uses an operating-system element only where
/// the platform leaves no other way. Here it leaves two: copy the link, or
/// send it to people the person already talks to.
///
/// Which items the menu holds is
/// ``CallScreenPresentation/participantsMenu(hasOthers:guest:)``; what
/// each invite item does, and what the screen says afterwards, is
/// ``CallInviteOffer``'s. This file only draws.

// MARK: - The menu

/// The menu's items. Drawn by the call screen over a scrim that closes it.
struct CallParticipantsMenu: View {
    @ObservedObject var offer: CallInviteOffer
    /// At least one other person is in the call.
    let hasOthers: Bool
    /// `Participants` was chosen: open the list of who is in the call.
    let onParticipants: () -> Void
    @ObservedObject var l10n = Localization.shared

    private var items: [CallScreenPresentation.ParticipantsMenuItem] {
        CallScreenPresentation.participantsMenu(hasOthers: hasOthers, guest: offer.guest)
    }

    var body: some View {
        VStack(spacing: 0) {
            ForEach(items, id: \.self) { item in
                switch item {
                case .participants:
                    row(l10n.t("call.control.participants"), icon: .person2, action: onParticipants)
                case .copyLink:
                    row(l10n.t("call.invite.menu_copy"), icon: .copy) {
                        Task { await offer.copyLink() }
                    }
                case .inviteContacts:
                    row(l10n.t("call.invite.menu_contact"), icon: .personPlus) {
                        offer.openPicker()
                    }
                }
            }
        }
        .padding(6)
        .frame(minWidth: 240)
        .fixedSize()
        .background(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .fill(Color.lakkoSurface))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                .strokeBorder(Color.lakkoBorder, lineWidth: 1))
        .shadow(color: .black.opacity(0.35), radius: 14, y: 6)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(l10n.t("call.invite.menu_a11y"))
    }

    private func row(
        _ title: String, icon: LkIcon, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                LkIconView(icon, size: 20, relativeTo: nil)
                    .frame(width: 24)
                Text(title)
                    .font(.body)
                Spacer(minLength: 0)
            }
            .foregroundStyle(Color.lakkoText)
            .padding(.horizontal, 12)
            .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - The contact list

/// `Invite contacts`: the person's contacts, by name the way the Contacts
/// list orders people, under the title `Invite to call`. Never conversations,
/// and never a group: an invitation is addressed to a person. Choosing a
/// contact sends the invitation into the one-to-one with them (founded first
/// when this device holds none), and the list STAYS OPEN so several people
/// can be invited: an invited row shows a check and `Invited` and cannot be
/// chosen twice, and the person closes the list with its own control. A
/// failure keeps the row pressable and says why; the call is untouched. With
/// no contacts, the list says so and points at `Copy invite link`.
///
/// A modifier rather than a `.sheet` inside the call screen because the offer
/// is optional, and an `@ObservedObject` cannot be: the inner modifier is what
/// observes it, and the outer one is what decides whether there is anything to
/// observe.
struct CallInvitePicker: ViewModifier {
    let offer: CallInviteOffer?

    @ViewBuilder
    func body(content: Content) -> some View {
        if let offer {
            content.modifier(Attached(offer: offer))
        } else {
            content
        }
    }

    private struct Attached: ViewModifier {
        @ObservedObject var offer: CallInviteOffer

        func body(content: Content) -> some View {
            content.sheet(
                isPresented: Binding(
                    get: { offer.pickerOpen },
                    set: { if !$0 { offer.closePicker() } })
            ) {
                CallInviteContactsScreen(
                    rows: offer.targets(),
                    notice: offer.pickerError,
                    invited: offer.invited,
                    requestSent: offer.requestSent,
                    onPick: { row in
                        Task {
                            await offer.sendInvite(
                                to: CallInviteTarget(id: row.id, title: row.name))
                        }
                    },
                    onClose: { offer.closePicker() })
            }
        }
    }
}

/// The list itself. Which rows it holds, their order and the search match are
/// ``InviteContacts``'; this only draws them.
struct CallInviteContactsScreen: View {

    /// Every contact, ordered, unfiltered.
    let rows: [InviteContactRow]
    /// One line above the list, in the danger colour: why the last invitation
    /// was not sent. The list stays open under it.
    var notice: String? = nil
    /// The rows already invited during this call (``InviteContactRow/id``).
    var invited: Set<String> = []
    /// The rows a contact request went to instead of the link
    /// (`docs/ux/who-can-find-me.md` 6.4).
    var requestSent: Set<String> = []
    let onPick: (InviteContactRow) -> Void
    let onClose: () -> Void

    @State private var searchText: String = ""
    @ObservedObject private var l10n = Localization.shared

    private var visible: [InviteContactRow] {
        InviteContacts.filter(rows, query: searchText)
    }

    /// What stands in for the list when it shows no row: the person has no
    /// contact at all, or the search matched none.
    static func placeholder(hasRows: Bool) -> String {
        Localization.text(hasRows ? "chats.search.no_results" : "call.invite.picker_empty")
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let notice {
                    Text(notice)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 8)
                        .accessibilityIdentifier(Self.noticeAccessibilityID)
                }
                if visible.isEmpty {
                    Text(Self.placeholder(hasRows: !rows.isEmpty))
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 24)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .padding(.top, 80)
                } else {
                    List {
                        ForEach(visible) { row in
                            let asked = requestSent.contains(row.id)
                            let done = invited.contains(row.id) || asked
                            Button { onPick(row) } label: {
                                HStack(spacing: 12) {
                                    InitialsAvatar(
                                        name: row.isPlaceholder ? "" : row.name,
                                        identity: row.id,
                                        size: 40,
                                        placeholderIcon: .personFill,
                                        photo: .person(row.key)
                                    )
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(row.name)
                                            .foregroundStyle(Color.lakkoText)
                                        if let line = row.usernameLine {
                                            Text(line)
                                                .font(.subheadline)
                                                .foregroundStyle(Color.lakkoTextMuted)
                                        }
                                    }
                                    Spacer(minLength: 8)
                                    if asked {
                                        Text(l10n.t("search.people.request_sent"))
                                            .font(.footnote.weight(.medium))
                                            .foregroundStyle(Color.lakkoTextMuted)
                                            .fixedSize()
                                    } else if done {
                                        Label(l10n.t("call.invite.row_invited"), icon: .checkmark, size: 14, relativeTo: .footnote)
                                            .font(.footnote.weight(.medium))
                                            .foregroundStyle(Color.lakkoTextMuted)
                                            .fixedSize()
                                    }
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(done)
                            .listRowSeparator(.hidden)
                            .listRowInsets(
                                EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                        }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle(l10n.t("call.invite.picker_title"))
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $searchText, prompt: Text(l10n.t("contacts.list.search")))
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.close"), action: onClose)
                }
            }
        }
        .accessibilityIdentifier(Self.accessibilityID)
    }

    static let accessibilityID = "call-invite-contacts"
    static let noticeAccessibilityID = "call-invite-contacts-notice"
}
