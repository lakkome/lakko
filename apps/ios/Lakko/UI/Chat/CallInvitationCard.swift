// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// A call invitation in a conversation (`docs/ux/call-links.md` section 3).
///
/// `Invite contacts` sends the call's link as a message, and a message whose
/// whole text is exactly one call link of this deployment
/// (``CallInviteLink/callInviteOf(_:ownOrigin:)``) is drawn as this: a phone
/// glyph, `Call invitation`, and a `Join` button that opens the call through
/// the app's own `/c/` link handling. It does not ring anyone; it is a
/// message.
///
/// Drawn inside the bubble, so it takes the bubble's foreground.
struct CallInvitationCard: View {
    /// Which bubble this sits in.
    let outgoing: Bool
    let onJoin: () -> Void

    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        HStack(spacing: 10) {
            LkIconView(.phoneFill, size: 20, relativeTo: nil)
                .frame(width: 36, height: 36)
                .background(
                    Circle().fill(
                        (outgoing ? Color.lakkoAccentForeground : Color.lakkoText).opacity(0.14)))
                .accessibilityHidden(true)
            Text(l10n.t("chat.call_invite.title"))
                .font(.body.weight(.semibold))
                .lineLimit(2)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: onJoin) {
                Text(l10n.t("chat.call.join_short"))
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(outgoing ? Color.lakkoAccent : Color.lakkoAccentForeground)
                    .padding(.horizontal, 14)
                    .frame(minHeight: 34)
                    .background(
                        Capsule().fill(outgoing ? Color.lakkoAccentForeground : Color.lakkoAccent))
                    // The touch target stays at 44 pt around the 34 pt pill.
                    .padding(.vertical, 5)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier(CallInvitationCard.joinAccessibilityID)
        }
        .frame(minWidth: 220)
        .accessibilityElement(children: .contain)
    }

    static let joinAccessibilityID = "call-invitation-join"
}
