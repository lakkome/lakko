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
/// ``CallScreenPresentation/participantsMenu(hasOthers:hasConversations:)``; what
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
        CallScreenPresentation.participantsMenu(
            hasOthers: hasOthers, hasConversations: !offer.targets().isEmpty)
    }

    var body: some View {
        VStack(spacing: 0) {
            ForEach(items, id: \.self) { item in
                switch item {
                case .participants:
                    row(l10n.t("call.control.participants"), systemImage: "person.2", action: onParticipants)
                case .copyLink:
                    row(l10n.t("call.invite.menu_copy"), systemImage: "doc.on.doc") {
                        Task { await offer.copyLink() }
                    }
                case .inviteContacts:
                    row(l10n.t("call.invite.menu_contact"), systemImage: "person.badge.plus") {
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
        _ title: String, systemImage: String, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: systemImage)
                    .font(.system(size: 17))
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

// MARK: - The conversation picker

/// `Invite contacts`: the list of conversations `Forward to` shows, under the
/// title `Invite to call`. Picking one sends the invitation into it, and the
/// list STAYS OPEN so several people can be invited: an invited row shows a
/// check and `Invited` and cannot be picked twice, and the person closes the
/// list with its own control. A failure keeps the row pressable and says why;
/// the call is untouched.
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
        @ObservedObject var l10n = Localization.shared

        func body(content: Content) -> some View {
            content.sheet(
                isPresented: Binding(
                    get: { offer.pickerOpen },
                    set: { if !$0 { offer.closePicker() } })
            ) {
                ForwardPickerScreen(
                    rows: offer.targets(),
                    title: l10n.t("call.invite.picker_title"),
                    notice: offer.pickerError,
                    doneIds: offer.invited,
                    doneLabel: l10n.t("call.invite.row_invited"),
                    cancelTitle: l10n.t("common.close"),
                    onPick: { row in
                        Task {
                            await offer.sendInvite(
                                to: CallInviteTarget(id: row.id, title: row.title))
                        }
                    },
                    onCancel: { offer.closePicker() })
            }
        }
    }
}
