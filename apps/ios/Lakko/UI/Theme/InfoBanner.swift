// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The design-system Banner in its informing form: an `info` icon, a short
/// title and a body on the sunken surface, with no action and no close
/// button. It asks nothing, so it is one accessibility element read title
/// then body, never a button and never focusable; the icon is decorative.
/// The text wraps at any dynamic type size.
struct InfoBanner: View {
    let title: String
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            LkIconView(.infoCircle, size: 17, relativeTo: .subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurfaceSunken)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("\(title). \(message)"))
        .accessibilityAddTraits(.isStaticText)
    }
}

/// The beta notice (`docs/ux/beta-notice.md`): shown on the welcome screen,
/// the sign-up screen and the guest's `Create account` form, until the
/// independent review of ADR-0046 is done.
struct BetaNoticeBanner: View {
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        InfoBanner(
            title: l10n.t(Self.titleKey),
            message: l10n.t(Self.bodyKey)
        )
    }

    static let titleKey = "beta.notice.title"
    static let bodyKey = "beta.notice.body"
}
