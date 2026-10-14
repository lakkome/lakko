// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `End`, for the person who started a group call
/// (`docs/ux/calls.md` section 4.1.2): leave, or end the call for everyone.
///
/// A menu of the app's own above the control bar, drawn by the call screen
/// over a scrim that closes it — the pattern of ``CallParticipantsMenu``, and
/// for the same reason never a system dialog. Whether `End` opens it at all is
/// ``EndForEveryone/endOpensMenu(isStarter:othersInCall:)``; this file only
/// draws. The menu is the confirmation: there is no second one.

// MARK: - The menu

struct CallEndMenu: View {
    /// This person leaves; the call goes on for the others.
    let onLeave: () -> Void
    /// The call ends for every person in it.
    let onEndForEveryone: () -> Void
    @ObservedObject var l10n = Localization.shared

    var body: some View {
        VStack(spacing: 0) {
            row(
                l10n.t("call.end_menu.leave"),
                systemImage: "rectangle.portrait.and.arrow.right",
                colour: Color.lakkoText,
                action: onLeave)
            // The destructive row in the end button's red, the only other red
            // on the call screen.
            row(
                l10n.t("call.end_menu.end_for_all"),
                systemImage: "phone.down.fill",
                colour: Color.lakkoDanger,
                action: onEndForEveryone)
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
        .accessibilityLabel(l10n.t("call.end_menu.a11y"))
    }

    private func row(
        _ title: String, systemImage: String, colour: Color, action: @escaping () -> Void
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
            .foregroundStyle(colour)
            .padding(.horizontal, 12)
            .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

// MARK: - The line for the others

/// "{name} ended the call for everyone", above whatever screen is showing for
/// a few seconds after the call's starter ended it. Drawn in the layout, in
/// the same top inset the link notice and the collapsed call use; never an
/// alert. The same line stands under `Call ended` while the call screen is
/// still up.
struct CallEndedForEveryoneBanner: View {
    let text: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "phone.down.fill")
                .foregroundStyle(Color.lakkoTextMuted)
                .padding(.top, 2)
                .accessibilityHidden(true)
            Text(text)
                .font(.headline)
                .foregroundStyle(Color.lakkoText)
                .lineLimit(3)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
        .accessibilityElement(children: .combine)
    }
}
